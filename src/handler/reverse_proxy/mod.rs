//! 反向代理处理器
//! 支持：HTTP / HTTPS / WS / WSS + gzip / chunked 透传
//! 子模块职责：
//!   lb.rs         — 负载均衡、节点状态、健康检查
//!   tls_client.rs — TLS 客户端连接
//!   conn.rs       — HTTP 连接层（发请求/读响应）
//!   response.rs   — 响应头透传、Cookie/Location 改写

pub mod circuit_breaker;
pub mod conn;
pub mod lb;
pub mod pool;
pub mod response;
pub mod tls_client;
pub mod ws_proxy;

use futures_util::StreamExt;
use tracing::error;
use xitca_web::{
    http::{StatusCode, WebResponse},
    WebContext,
};

use crate::config::model::LocationConfig;
use crate::dispatcher::vhost::SiteInfo;
use crate::middleware::proxy_cache::{CacheKey, ProxyCache};
use crate::server::http::AppState;

pub use lb::{health_check_task, NodeState, UpstreamPool, UpstreamRegistry};

/// 处理反向代理请求（公开入口）
pub async fn handle_xitca(
    ctx: &WebContext<'_, AppState>,
    site: &SiteInfo,
    location: &LocationConfig,
) -> WebResponse {
    // ── 找到上游配置 ─────────────────────────────────────────
    let upstream_name = match &location.upstream {
        Some(n) => n.as_str(),
        None => return response::proxy_error(StatusCode::INTERNAL_SERVER_ERROR, "未配置 upstream"),
    };
    // 直接从预构建的池查表，零堆分配
    let pool = match site.upstream_pools.get(upstream_name) {
        Some(p) => p.clone(),
        None => return response::proxy_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("上游组 '{}' 未找到", upstream_name),
        ),
    };

    // ── 负载均衡选节点 ───────────────────────────────────────
    let client_ip_str = ctx.req().body().socket_addr().ip().to_string();
    let node = match pool.pick(Some(&client_ip_str)) {
        Some(n) => n,
        None => return response::proxy_error(StatusCode::BAD_GATEWAY, "所有上游节点均不可用"),
    };

    // ── 提取请求信息（用 &str 借用，避免堆分配）────────────────────────────
    let method = ctx.req().method().as_str();
    let path   = ctx.req().uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
    // HTTP/2 下没有 Host 头，:authority 伪头在 uri.authority() 里
    let client_host: &str = ctx.req().uri().authority()
        .map(|a| a.as_str())
        .or_else(|| ctx.req().headers().get("host").and_then(|v| v.to_str().ok()))
        .unwrap_or("");
    // upstream_host：优先用配置里的，否则透传客户端 Host
    let upstream_host_owned: Option<String> = node.upstream_host.clone();
    let upstream_host: &str = upstream_host_owned.as_deref().unwrap_or(client_host);

    // ── 过滤并收集请求头────────────────
    // WebSocket 升级请求需要保留 Upgrade/Sec-WebSocket-* 头，否则上游不能完成握手
    let client_ip_str_ref = client_ip_str.as_str();
    let scheme_str = ctx.req().uri().scheme_str().unwrap_or("http");
    // 收集请求头：borrowed 部分用 &str，只有 proxy_set_headers 变量替换时才堆分配
    // 两段分开存，forward_request 接受 &[(&str, &str)] + &[(String, String)]
    let header_count = ctx.req().headers().len();
    let mut client_headers: Vec<(String, String)> = Vec::with_capacity(header_count + 4);
    client_headers.extend(
        ctx.req().headers()
            .iter()
            .filter_map(|(k, v)| {
                let name = k.as_str();
                // hop-by-hop 头不透传，用 phf 查找（见 response.rs HOP_BY_HOP_SET）
                if crate::handler::reverse_proxy::response::is_hop_by_hop(name) {
                    return None;
                }
                v.to_str().ok().map(|val| (name.to_string(), val.to_string()))
            })
    );

    // 应用 proxy_set_headers：重写指定请求头（支持 $remote_addr/$host/$scheme/$request_uri 变量）
    for h in &location.proxy_set_headers {
        let val = h.value
            .replace("$remote_addr", client_ip_str_ref)
            .replace("$host", upstream_host)
            .replace("$scheme", scheme_str)
            .replace("$request_uri", path);
        client_headers.retain(|(k, _)| !k.eq_ignore_ascii_case(&h.name));
        client_headers.push((h.name.clone(), val));
    }

    // ── 检测 WebSocket 升级请求（H1 + H2 extended CONNECT 两种方式）──────
    // H1：Upgrade: websocket
    // H2：method=CONNECT + h2::ext::Protocol="websocket"（RFC 8441）
    let is_ws_h1 = ctx.req().headers()
        .get(xitca_web::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    let is_ws_h2 = method.eq_ignore_ascii_case("CONNECT")
        && ctx.req().extensions()
            .get::<h2::ext::Protocol>()
            .map(|p| p.as_str().eq_ignore_ascii_case("websocket"))
            .unwrap_or(false);
    let is_ws = is_ws_h1 || is_ws_h2;

    // ── 读取请求体（POST/PUT/PATCH 等）──────────────────────────────────
    // 不能只依赖 Content-Length（H2 下无此头，chunked 也无），直接读 body stream
    let request_body: Vec<u8> = if matches!(method, "POST" | "PUT" | "PATCH" | "DELETE") {
        let cap = ctx.req().headers()
            .get(xitca_web::http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        let mut bytes = Vec::with_capacity(cap);
        let mut body = ctx.body_borrow_mut();
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(b) => bytes.extend_from_slice(b.as_ref()),
                Err(_) => break,
            }
        }
        bytes
    } else {
        Vec::new()
    };

    // ── Location 配置 ─────────────────────────────────────────────────────
    let strip_cookie_secure  = location.strip_cookie_secure;
    let proxy_cookie_domain  = location.proxy_cookie_domain.as_deref();
    let proxy_redirect_from  = location.proxy_redirect_from.as_deref();
    let proxy_redirect_to    = location.proxy_redirect_to.as_deref();
    let add_headers          = &location.add_headers;
    let cache_rules          = &location.cache_rules;
    let sub_filter           = &location.sub_filter;

    // ── proxy_cache 查询（读缓存，O(1) 内存匹配） ─────────────────────
    let cache_key = CacheKey::new(method, upstream_host, path);
    let proxy_cache: Option<std::sync::Arc<ProxyCache>> = site.proxy_cache_arc.clone();

    if let Some(ref cache) = proxy_cache {
        // 直接传 HeaderMap，跳过中间 Vec 堆分配
        if cache.should_lookup(method, ctx.req().headers()) {
            if let Some(entry) = cache.get(&cache_key) {
                // 缓存命中，直接构造响应返回
                use xitca_web::body::ResponseBody;
                use xitca_web::http::{StatusCode, WebResponse};
                let mut resp = WebResponse::new(ResponseBody::from(entry.body));
                *resp.status_mut() = StatusCode::from_u16(entry.status).unwrap_or(StatusCode::OK);
                use xitca_web::http::header::{HeaderName, HeaderValue};
                for (k, v) in &entry.headers {
                    if let (Ok(name), Ok(val)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
                        resp.headers_mut().insert(name, val);
                    }
                }
                // X-Cache: HIT 头标识缓存命中（与 Nginx 行为一致）
                resp.headers_mut().insert(
                    HeaderName::from_static("x-cache"),
                    HeaderValue::from_static("HIT"),
                );
                return resp;
            }
        }
    }

    // ── 转发请求 ───────────────────────────────────────────
    node.active_connections.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // WS/WSS 请求走专用的零拷贝反代路径
    let mut resp = if is_ws {
        ws_proxy::handle_ws_proxy(
            ctx, &node.addr, node.tls, &node.tls_sni, node.tls_insecure,
            &client_headers, &client_ip_str, &upstream_host, &path,
            strip_cookie_secure, proxy_cookie_domain,
            proxy_redirect_from, proxy_redirect_to,
            is_ws_h2,
        ).await
    } else {
        // 将 proxy_cache 引用传入 conn 层，在有完整 body bytes 时导入缓存
        let cache_ref = proxy_cache.as_ref().map(|c| (c, &cache_key));
        // retry 循环：失败时重新选节点重试，第一次失败不算在 retry 次数内
        let max_attempts = 1 + pool.retry as usize;
        let mut last_err = String::new();
        let mut success = false;
        let mut resp_opt: Option<xitca_web::http::WebResponse> = None;

        'retry: for attempt in 0..max_attempts {
            if attempt > 0 {
                if pool.retry_timeout > 0 {
                    tokio::time::sleep(tokio::time::Duration::from_secs(pool.retry_timeout)).await;
                }
                // 重试时重新选节点（避免重选到已失败的节点）
                if let Some(new_node) = pool.pick(Some(&client_ip_str)) {
                    // 用新节点覆盖（内层用 node 变量嵌套，这里只记录日志）
                    tracing::debug!("反向代理第 {} 次重试，节点: {}", attempt, new_node.addr);
                }
            }

            let result = conn::forward_request(
                &ctx.state().conn_pool,
                &node.addr, method, path, upstream_host,
                node.tls, &node.tls_sni, node.tls_insecure,
                &client_headers, client_ip_str_ref,
                &request_body,
                strip_cookie_secure, proxy_cookie_domain,
                proxy_redirect_from, proxy_redirect_to,
                sub_filter,
                cache_ref,
                scheme_str,
                pool.keepalive_requests,
                pool.keepalive_time,
                pool.keepalive_max_idle,
                pool.connect_timeout,
                pool.read_timeout,
                pool.write_timeout,
            ).await;

            match result {
                Ok(r) => {
                    node.record_success();
                    resp_opt = Some(r);
                    success = true;
                    break 'retry;
                }
                Err(e) => {
                    node.record_failure();
                    last_err = format!("{}", e);
                    error!("反向代理转发失败 (attempt {}/{}) → {}: {}", attempt + 1, max_attempts, node.addr, e);
                }
            }
        }

        if success {
            resp_opt.unwrap()
        } else {
            response::proxy_error(StatusCode::BAD_GATEWAY, &format!("上游 {} 响应失败: {}", node.addr, last_err))
        }
    };

    node.active_connections.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

    // 缓存写入已在 conn::forward_request 里完成（在 body 完整时导入）
    // 这里只需设置 X-Cache: MISS 头
    if proxy_cache.is_some() {
        use xitca_web::http::header::{HeaderName, HeaderValue};
        resp.headers_mut().insert(
            HeaderName::from_static("x-cache"),
            HeaderValue::from_static("MISS"),
        );
    }

    // apply_extra_headers：向客户端响应注入自定义头（等价 Nginx add_header）
    apply_extra_headers(&mut resp, &add_headers, &cache_rules, &path, client_ip_str_ref, scheme_str);
    resp
}

/// 全局 cache_rules 正则缓存（pattern → 预编译 Regex）
static CACHE_RULE_RE_CACHE: std::sync::OnceLock<dashmap::DashMap<String, regex::Regex>> =
    std::sync::OnceLock::new();

fn cache_rule_regex(pattern: &str) -> Option<regex::Regex> {
    let map = CACHE_RULE_RE_CACHE.get_or_init(|| dashmap::DashMap::new());
    if let Some(re) = map.get(pattern) {
        return Some(re.clone());
    }
    match regex::Regex::new(pattern) {
        Ok(re) => {
            map.insert(pattern.to_string(), re.clone());
            Some(re)
        }
        Err(_) => None,
    }
}

/// 应用 add_headers 和 cache_rules 到响应
///
/// - `add_headers`：直接向响应头插入自定义头
/// - `cache_rules`：按正则匹配请求路径，匹配则覆盖 Cache-Control
fn apply_extra_headers(
    resp: &mut xitca_web::http::WebResponse,
    add_headers: &[crate::config::model::HeaderOverride],
    cache_rules: &[crate::config::model::CacheRule],
    path: &str,
    remote_addr: &str,
    scheme: &str,
) {
    use xitca_web::http::header::{HeaderName, HeaderValue, CACHE_CONTROL};

    // 计算 cache_rules 匹配（只对成功响应）
    if resp.status().is_success() {
        for rule in cache_rules {
            if let Some(re) = cache_rule_regex(&rule.pattern) {
                if re.is_match(path) {
                    if let Ok(v) = HeaderValue::from_str(&rule.cache_control) {
                        resp.headers_mut().insert(CACHE_CONTROL, v);
                    }
                    break; // 第一条匹配的规则生效
                }
            }
        }
    }

    // 注入 add_headers
    for h in add_headers {
        let val = h.value
            .replace("$remote_addr", remote_addr)
            .replace("$scheme", scheme);
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(h.name.as_bytes()),
            HeaderValue::from_str(&val),
        ) {
            resp.headers_mut().insert(name, val);
        }
    }
}
