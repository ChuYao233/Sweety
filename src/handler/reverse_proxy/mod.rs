//! 反向代理处理器
//! 支持：HTTP / HTTPS / WS / WSS + gzip / chunked 透传
//! 子模块职责：
//!   lb.rs         — 负载均衡、节点状态、健康检查
//!   tls_client.rs — TLS 客户端连接
//!   conn.rs       — HTTP 连接层（发请求/读响应）
//!   response.rs   — 响应头透传、Cookie/Location 改写

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
    // ── 找到上游配置 ─────────────────────────────────────────────────────
    let upstream_name = match &location.upstream {
        Some(n) => n.clone(),
        None => return response::proxy_error(StatusCode::INTERNAL_SERVER_ERROR, "未配置 upstream"),
    };
    let upstream_cfg = match site.upstreams.iter().find(|u| u.name == upstream_name) {
        Some(u) => u.clone(),
        None => return response::proxy_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("上游组 '{}' 未找到", upstream_name),
        ),
    };

    // ── 负载均衡选节点 ───────────────────────────────────────────────────
    let pool = UpstreamPool::from_config(&upstream_cfg);
    let client_ip_str = ctx.req().body().socket_addr().ip().to_string();
    let node = match pool.pick(Some(&client_ip_str)) {
        Some(n) => n,
        None => return response::proxy_error(StatusCode::BAD_GATEWAY, "所有上游节点均不可用"),
    };

    // ── 提取请求信息 ─────────────────────────────────────────────────────
    let method = ctx.req().method().as_str().to_string();
    let path   = ctx.req().uri().path_and_query().map(|p| p.as_str()).unwrap_or("/").to_string();
    let client_host = ctx.req().headers().get("host")
        .and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let upstream_host = node.upstream_host.clone().unwrap_or_else(|| client_host.clone());

    // ── 过滤并收集请求头──────────────────────────────
    // WebSocket 升级请求需要保留 Upgrade/Sec-WebSocket-* 头，否则上游不能完成握手
    let client_ip_str_ref = client_ip_str.as_str();
    let scheme_str = ctx.req().uri().scheme_str().unwrap_or("http");
    let mut client_headers: Vec<(String, String)> = ctx.req().headers()
        .iter()
        .filter_map(|(k, v)| {
            let name = k.as_str().to_lowercase();
            // hop-by-hop 头不透传（WebSocket 升级相关头保留）
            if matches!(name.as_str(),
                "host" | "connection" | "proxy-connection" | "transfer-encoding" | "te" | "trailer"
            ) {
                return None;
            }
            v.to_str().ok().map(|val| (k.as_str().to_string(), val.to_string()))
        })
        .collect();

    // 应用 proxy_set_headers：重写指定请求头（支持 $remote_addr/$host/$scheme/$request_uri 变量）
    for h in &location.proxy_set_headers {
        let val = h.value
            .replace("$remote_addr", client_ip_str_ref)
            .replace("$host", &upstream_host)
            .replace("$scheme", scheme_str)
            .replace("$request_uri", &path);
        // 删除同名旧头，再插入（高效覆盖）
        let lower = h.name.to_lowercase();
        client_headers.retain(|(k, _)| k.to_lowercase() != lower);
        client_headers.push((h.name.clone(), val));
    }

    // ── 检测 WebSocket 升级请求（H1 + H2 extended CONNECT 两种方式）──────
    // H1：Upgrade: websocket
    // H2：method=CONNECT + h2::ext::Protocol="websocket"（RFC 8441）
    let is_ws_h1 = ctx.req().headers()
        .get(xitca_web::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_lowercase().contains("websocket"))
        .unwrap_or(false);
    let is_ws_h2 = method == "CONNECT"
        && ctx.req().extensions()
            .get::<h2::ext::Protocol>()
            .map(|p| p.as_str().eq_ignore_ascii_case("websocket"))
            .unwrap_or(false);
    let is_ws = is_ws_h1 || is_ws_h2;

    // ── 读取请求体（POST/PUT/PATCH 等）──────────────────────────────────
    // 不能只依赖 Content-Length（H2 下无此头，chunked 也无），直接读 body stream
    let request_body: Vec<u8> = if matches!(method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
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
    let proxy_cookie_domain  = location.proxy_cookie_domain.clone();
    let proxy_redirect_from  = location.proxy_redirect_from.clone();
    let proxy_redirect_to    = location.proxy_redirect_to.clone();
    let add_headers          = location.add_headers.clone();
    let cache_rules          = location.cache_rules.clone();
    let sub_filter           = location.sub_filter.clone();

    // ── proxy_cache 查询（读缓存，O(1) 内存匹配） ─────────────────────
    let cache_key = CacheKey::new(&method, &upstream_host, &path);
    let proxy_cache: Option<std::sync::Arc<ProxyCache>> = ctx.state()
        .proxy_caches
        .get(&site.name)
        .cloned();

    if let Some(ref cache) = proxy_cache {
        // 过滤请求头列表用于 bypass 判断
        let req_headers_for_cache: Vec<(String, String)> = ctx.req().headers().iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string())))
            .collect();
        if cache.should_lookup(&method, &req_headers_for_cache) {
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
            strip_cookie_secure, proxy_cookie_domain.as_deref(),
            proxy_redirect_from.as_deref(), proxy_redirect_to.as_deref(),
            is_ws_h2,
        ).await
    } else {
        // 将 proxy_cache 引用传入 conn 层，在有完整 body bytes 时导入缓存
        let cache_ref = proxy_cache.as_ref().map(|c| (c, &cache_key));
        let result = conn::forward_request(
            &ctx.state().conn_pool,
            &node.addr, &method, &path, &upstream_host,
            node.tls, &node.tls_sni, node.tls_insecure,
            &client_headers, &client_ip_str,
            &request_body,
            strip_cookie_secure, proxy_cookie_domain.as_deref(),
            proxy_redirect_from.as_deref(), proxy_redirect_to.as_deref(),
            &sub_filter,
            cache_ref,
        ).await;

        match result {
            Ok(r) => { node.fail_count.store(0, std::sync::atomic::Ordering::Relaxed); r }
            Err(e) => {
                node.fail_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if node.fail_count.load(std::sync::atomic::Ordering::Relaxed) >= 3 {
                    node.mark_unhealthy();
                }
                error!("反向代理转发失败 → {}: {}", node.addr, e);
                response::proxy_error(StatusCode::BAD_GATEWAY, &format!("上游 {} 响应失败", node.addr))
            }
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
            if let Ok(re) = regex::Regex::new(&rule.pattern) {
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
