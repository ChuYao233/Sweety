//! 反向代理处理器
//! 支持：HTTP / HTTPS / WS / WSS + gzip / chunked 透传
//! 子模块职责：
//!   lb.rs           — 负载均衡、节点状态、健康检查
//!   tls_client.rs   — TLS 客户端连接
//!   conn.rs         — HTTP/1.1 连接层（发请求/读响应）
//!   upstream_h2.rs  — HTTP/2 上游连接池（h2c / h2 over TLS）
//!   response.rs     — 响应头透传、Cookie/Location 改写

pub mod circuit_breaker;
pub mod conn;
pub mod error;
pub mod lb;
pub mod pool;
pub mod response;
pub mod tls_client;
pub mod upstream_h2;
pub mod ws_proxy;

use tracing::error;
use sweety_web::{
    http::{StatusCode, WebResponse},
    WebContext,
};

use crate::config::model::LocationConfig;
use crate::dispatcher::vhost::SiteInfo;
use crate::middleware::proxy_cache::{CacheKey, ProxyCache};
use crate::server::http::AppState;

pub use lb::{health_check_task, NodeState, UpstreamPool, UpstreamRegistry};

/// 处理反向代理请求（公开入口）
pub async fn handle_sweety(
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

    // ── 非 WS CONNECT 早期拦截（与 Nginx 行为一致）───────────────────────
    // Nginx 作为反向代理不支持 HTTP CONNECT 隧道，直接拒绝非 WebSocket 的 CONNECT 请求
    // is_h2_ws() 由框架 H2 dispatcher 在接收请求时检测 h2::ext::Protocol 写入，可靠
    {
        let m = ctx.req().method().as_str();
        if m.eq_ignore_ascii_case("CONNECT") && !ctx.req().body().is_h2_ws() {
            // 纯隧道 CONNECT，Nginx 也不支持，返回 400
            return response::proxy_error(StatusCode::BAD_REQUEST, "CONNECT tunneling not supported");
        }
    }

    // ── 负载均衡选节点 ───────────────────────────────────────
    let client_ip_str = ctx.req().body().socket_addr().ip().to_string();
    let node = match pool.pick(Some(&client_ip_str)) {
        Some(n) => n,
        None => return response::proxy_error(StatusCode::BAD_GATEWAY, "所有上游节点均不可用"),
    };

    // ── 提取请求信息（用 &str 借用，避免堆分配）────────────────────────────
    let method = ctx.req().method().as_str();
    // H2 CONNECT（extended CONNECT / WS）的 URI 是 authority-form，path_and_query() 可能为空
    // 此时用 uri.path()，H1 WS 的 GET 请求 path_and_query() 正常有值
    let path   = ctx.req().uri().path_and_query().map(|p| p.as_str())
        .filter(|p| !p.is_empty() && *p != "/")
        .unwrap_or_else(|| ctx.req().uri().path());
    // HTTP/2 下没有 Host 头，:authority 伪头在 uri.authority() 里
    let client_host: &str = ctx.req().uri().authority()
        .map(|a| a.as_str())
        .or_else(|| ctx.req().headers().get("host").and_then(|v| v.to_str().ok()))
        .unwrap_or("");
    // upstream_host：优先用配置里的，否则透传客户端 Host
    let upstream_host_owned: Option<String> = node.upstream_host.clone();
    let upstream_host: &str = upstream_host_owned.as_deref().unwrap_or(client_host);

    // ── 检测 WebSocket 升级请求（H1 + H2 extended CONNECT 两种方式）──────
    // H2 WS：框架在 dispatcher 层已检测 h2::ext::Protocol，通过 RequestExt::is_h2_ws() 可靠传递
    // H1 WS：GET + Upgrade: websocket（RFC 6455）
    // 非 WS CONNECT（正向代理隧道）不命中，在入口已拦截返回 400
    let is_ws_h2 = ctx.req().body().is_h2_ws();
    let is_ws_h1 = !is_ws_h2
        && ctx.req().headers()
            .get(sweety_web::http::header::UPGRADE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.eq_ignore_ascii_case("websocket"))
            .unwrap_or(false);
    let is_ws = is_ws_h1 || is_ws_h2;

    // ── 过滤并收集请求头────────────────
    // WebSocket 升级请求需要保留 Upgrade/Sec-WebSocket-* 头，否则上游不能完成握手
    // 对标 Nginx: proxy_set_header Upgrade $http_upgrade; proxy_set_header Connection "upgrade";
    let client_ip_str_ref = client_ip_str.as_str();
    let scheme_str = ctx.req().uri().scheme_str().unwrap_or("http");
    let header_count = ctx.req().headers().len();
    let mut client_headers: Vec<(String, String)> = Vec::with_capacity(header_count + 4);
    client_headers.extend(
        ctx.req().headers()
            .iter()
            .filter_map(|(k, v)| {
                let name = k.as_str();
                // WS 请求保留 Upgrade 和 Sec-WebSocket-* 头（不被 hop-by-hop 过滤）
                let is_ws_header = is_ws
                    && (name.eq_ignore_ascii_case("upgrade")
                        || name.to_ascii_lowercase().starts_with("sec-websocket-"));
                if !is_ws_header && crate::handler::reverse_proxy::response::is_hop_by_hop(name) {
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

    // ── 请求体流式透传（POST/PUT/PATCH/DELETE）────────────────────────────
    // 不全量 collect 到内存：大文件上传零内存拷贝，chunked body 直接 pipe 给上游
    // 100-continue 处理、流式写上游均在 conn::forward_request 内完成
    // GET/HEAD 等无 body 方法：take_body_ref() 返回 RequestBody::None，conn 层会跳过发送
    let request_body = ctx.take_body_ref();

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
                use sweety_web::body::ResponseBody;
                use sweety_web::http::{StatusCode, WebResponse};
                let mut resp = WebResponse::new(ResponseBody::from(entry.body));
                *resp.status_mut() = StatusCode::from_u16(entry.status).unwrap_or(StatusCode::OK);
                use sweety_web::http::header::{HeaderName, HeaderValue};
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
            &client_headers, &client_ip_str, upstream_host, path,
            strip_cookie_secure, proxy_cookie_domain,
            proxy_redirect_from, proxy_redirect_to,
            is_ws_h2,
        ).await
    } else if node.http2 {
        // ── HTTP/2 上游路径（h2c 或 h2 over TLS）────────────────────────────
        let h2_pool = ctx.state().h2_pools.get_or_create(
            &node.addr, node.tls, &node.tls_sni, node.tls_insecure,
            8, // 每节点最多 8 条 H2 连接，每条多路复用 stream
            pool.connect_timeout,
        );
        match forward_request_h2(
            h2_pool,
            method, path, upstream_host,
            &client_headers, client_ip_str_ref, scheme_str,
            request_body,
            strip_cookie_secure, proxy_cookie_domain,
            proxy_redirect_from, proxy_redirect_to,
            pool.read_timeout,
        ).await {
            Ok(r) => { node.record_success(); r }
            Err(e) => {
                node.record_failure();
                error!("H2 反向代理失败 → {}: {}", node.addr, e);
                response::proxy_error(StatusCode::BAD_GATEWAY, &format!("上游 H2 {} 失败: {}", node.addr, e))
            }
        }
    } else {
        // 将 proxy_cache 引用传入 conn 层，在有完整 body bytes 时导入缓存
        let cache_ref = proxy_cache.as_ref().map(|c| (c, &cache_key));
        // retry 循环：上游级别重试（节点故障），conn::forward_request 内部处理 idle 连接重试
        // body 流只能消耗一次，上游级别重试只在首次（body 尚未消费）时有效
        let max_attempts = 1 + pool.retry as usize;
        let mut last_err = String::new();
        let mut resp_opt: Option<sweety_web::http::WebResponse> = None;
        let mut body_for_retry = Some(request_body);

        'retry: for attempt in 0..max_attempts {
            if attempt > 0 {
                // body 已消费则无法重试（大文件上传场景）
                if body_for_retry.is_none() { break 'retry; }
                if pool.retry_timeout > 0 {
                    tokio::time::sleep(tokio::time::Duration::from_secs(pool.retry_timeout)).await;
                }
                if let Some(new_node) = pool.pick(Some(&client_ip_str)) {
                    tracing::debug!("反向代理第 {} 次重试，节点: {}", attempt, new_node.addr);
                }
            }

            let result = conn::forward_request(
                &ctx.state().conn_pool,
                &node.addr, method, path, upstream_host,
                node.tls, &node.tls_sni, node.tls_insecure,
                &client_headers, client_ip_str_ref,
                body_for_retry.take().unwrap_or_default(),
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
                location.proxy_buffering,
            ).await;

            match result {
                Ok(r) => {
                    node.record_success();
                    resp_opt = Some(r);
                    break 'retry;
                }
                Err(e) => {
                    node.record_failure();
                    last_err = format!("{}", e);
                    error!("反向代理转发失败 (attempt {}/{}) → {}: {}", attempt + 1, max_attempts, node.addr, e);
                }
            }
        }

        resp_opt.unwrap_or_else(|| {
            response::proxy_error(StatusCode::BAD_GATEWAY, &format!("上游 {} 响应失败: {}", node.addr, last_err))
        })
    };

    node.active_connections.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

    // 缓存写入已在 conn::forward_request 里完成（在 body 完整时导入）
    // 这里只需设置 X-Cache: MISS 头
    if proxy_cache.is_some() {
        use sweety_web::http::header::{HeaderName, HeaderValue};
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
    resp: &mut sweety_web::http::WebResponse,
    add_headers: &[crate::config::model::HeaderOverride],
    cache_rules: &[crate::config::model::CacheRule],
    path: &str,
    remote_addr: &str,
    scheme: &str,
) {
    use sweety_web::http::header::{HeaderName, HeaderValue, CACHE_CONTROL};

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

/// HTTP/2 上游转发
///
/// - 全量 collect 请求体（gRPC 通常较小；大文件上传走 H1 路径）
/// - 响应体流式透传（bounded channel，背压传递给客户端）
#[allow(clippy::too_many_arguments)]
async fn forward_request_h2(
    pool: std::sync::Arc<upstream_h2::H2NodePool>,
    method: &str,
    path: &str,
    host: &str,
    extra_headers: &[(String, String)],
    client_ip: &str,
    scheme: &str,
    req_body: sweety_web::body::RequestBody,
    strip_cookie_secure: bool,
    proxy_cookie_domain: Option<&str>,
    proxy_redirect_from: Option<&str>,
    proxy_redirect_to: Option<&str>,
    read_timeout_secs: u64,
) -> anyhow::Result<WebResponse> {
    use bytes::Bytes;
    use futures_util::StreamExt;
    use sweety_web::body::ResponseBody;
    use sweety_web::http::header::{HeaderName, HeaderValue, CONTENT_LENGTH};

    let read_timeout = std::time::Duration::from_secs(if read_timeout_secs > 0 { read_timeout_secs } else { 60 });

    // 构造 HTTP/2 请求
    use sweety_web::http::{Version, request::Builder as ReqBuilder};
    let uri_str = format!("https://{}{}", host, path);
    let uri: sweety_web::http::Uri = uri_str.parse().unwrap_or_else(|_| {
        format!("https://localhost{}", path).parse().unwrap()
    });

    let mut builder = ReqBuilder::new()
        .method(method)
        .uri(uri)
        .version(Version::HTTP_2);

    // 透传请求头，跳过 hop-by-hop
    for (k, v) in extra_headers {
        if response::is_hop_by_hop(k) { continue; }
        if k.eq_ignore_ascii_case("content-length") { continue; }
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            builder = builder.header(name, val);
        }
    }
    builder = builder
        .header("x-real-ip", client_ip)
        .header("x-forwarded-for", client_ip)
        .header("x-forwarded-proto", scheme);

    // 全量 collect 请求体（H2 stream 层已有流量控制）
    let body_bytes: Option<Bytes> = {
        let mut body = req_body;
        let mut buf = bytes::BytesMut::new();
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(b) => buf.extend_from_slice(&b),
                Err(_) => break,
            }
        }
        if buf.is_empty() { None } else { Some(buf.freeze()) }
    };

    let req = builder.body(()).map_err(|e| anyhow::anyhow!("h2 build request: {e}"))?;
    let (parts, recv_stream) = pool.send(req, body_bytes).await?;

    let status = StatusCode::from_u16(parts.status.as_u16()).unwrap_or(StatusCode::OK);

    // 收集响应头（透传给客户端）
    let mut resp_headers: Vec<(String, String)> = Vec::with_capacity(parts.headers.len());
    for (k, v) in &parts.headers {
        if let Ok(vs) = v.to_str() {
            resp_headers.push((k.as_str().to_string(), vs.to_string()));
        }
    }

    // 304/204/205/1xx 无 body
    let no_body = status == StatusCode::NOT_MODIFIED
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::RESET_CONTENT
        || status.is_informational();

    if no_body {
        let mut resp = WebResponse::new(ResponseBody::none());
        *resp.status_mut() = status;
        response::apply_response_headers(&mut resp, &resp_headers,
            strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to);
        return Ok(resp);
    }

    // 流式透传响应体（bounded channel，背压）
    // spawn_local：在同一 worker 线程驱动，避免跨线程调度开销
    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<Bytes>>(4);
    tokio::task::spawn_local(async move {
        let mut stream = recv_stream;
        loop {
            match tokio::time::timeout(read_timeout, stream.data()).await {
                Ok(Some(Ok(data))) => {
                    let _ = stream.flow_control().release_capacity(data.len());
                    if tx.send(Ok(data)).await.is_err() { break; }
                }
                Ok(Some(Err(e))) => {
                    let _ = tx.send(Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string()))).await;
                    break;
                }
                Ok(None) => break,
                Err(_) => {
                    let _ = tx.send(Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "h2 上游响应体读取超时"))).await;
                    break;
                }
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let mut resp = WebResponse::new(ResponseBody::box_stream(stream));
    *resp.status_mut() = status;
    response::apply_response_headers(&mut resp, &resp_headers,
        strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to);

    // H2 响应无 Transfer-Encoding，保留 Content-Length（若上游提供）
    resp.headers_mut().remove(sweety_web::http::header::TRANSFER_ENCODING);
    if let Some(cl) = parts.headers.get(CONTENT_LENGTH) {
        resp.headers_mut().insert(CONTENT_LENGTH, cl.clone());
    }

    Ok(resp)
}
