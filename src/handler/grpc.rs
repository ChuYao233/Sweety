//! gRPC 反向代理处理器
//!
//! gRPC = HTTP/2 + 二进制 Protobuf 帧 + Content-Type: application/grpc
//!
//! # 实现说明
//! sweety-web 底层是 HTTP/1.1 连接层，直接透传 gRPC H2 帧不可行。
//! 实际方案：通过 HTTP/1.1 连接将 gRPC 请求转发给上游（适用于支持 h2c 的 gRPC 代理）。
//! 对于生产 gRPC，推荐上游监听 HTTP/2 cleartext（h2c），Sweety 通过 HTTP/1.1 全量收集转发。
//!
//! # 与普通反代的区别
//! 1. 强制 Content-Type: application/grpc
//! 2. 响应头追加 grpc-status、grpc-message Trailer
//! 3. 超时默认更长（gRPC streaming 可能跑秒级）
//! 4. 不做 body 压缩/替换（Protobuf 二进制，替换无意义）

use tracing::debug;
use sweety_web::{
    body::ResponseBody,
    http::{StatusCode, WebResponse, header::{CONTENT_TYPE, HeaderValue}},
    WebContext,
};

use crate::config::model::LocationConfig;
use crate::dispatcher::vhost::SiteInfo;
use crate::server::http::AppState;

/// 处理 gRPC 反向代理请求
pub async fn handle_sweety(
    ctx: &WebContext<'_, AppState>,
    site: &SiteInfo,
    location: &LocationConfig,
) -> WebResponse {
    use futures_util::StreamExt;

    // ── 找到上游（直接用预构建的池，零堆分配） ───────────────────────
    let upstream_name = match &location.upstream {
        Some(n) => n.as_str(),
        None => return grpc_error(StatusCode::INTERNAL_SERVER_ERROR, 13, "未配置 upstream"),
    };
    let pool = match site.upstream_pools.get(upstream_name) {
        Some(p) => p.clone(),
        None => return grpc_error(
            StatusCode::INTERNAL_SERVER_ERROR, 13,
            &format!("上游组 '{}' 未找到", upstream_name),
        ),
    };

    let client_ip = ctx.req().body().socket_addr().ip().to_string();
    let node = match pool.pick(Some(&client_ip)) {
        Some(n) => n,
        None => return grpc_error(StatusCode::BAD_GATEWAY, 14, "所有上游节点均不可用"),
    };

    // ── 提取请求信息（全部用 &str 引用，避免堆分配） ───────────────────
    let method = ctx.req().method().as_str();
    let path = ctx.req().uri().path_and_query()
        .map(|p| p.as_str()).unwrap_or("/");
    // HTTP/2 下没有 Host 头，:authority 伪头在 uri.authority() 里
    let client_host_owned: String = ctx.req().uri().authority()
        .map(|a| a.as_str().to_string())
        .or_else(|| ctx.req().headers().get("host").and_then(|v| v.to_str().ok()).map(|s| s.to_string()))
        .unwrap_or_default();
    let upstream_host: std::borrow::Cow<str> = node.upstream_host.as_deref()
        .map(std::borrow::Cow::Borrowed)
        .unwrap_or(std::borrow::Cow::Owned(client_host_owned));

    // ── 收集请求头（过滤 hop-by-hop）──────────────────────────────────
    let hdr_count = ctx.req().headers().len();
    let mut req_headers: Vec<(String, String)> = Vec::with_capacity(hdr_count + 4);
    req_headers.extend(
        ctx.req().headers().iter()
            .filter_map(|(k, v)| {
                let name = k.as_str();
                if name.eq_ignore_ascii_case("host")
                    || name.eq_ignore_ascii_case("connection")
                    || name.eq_ignore_ascii_case("proxy-connection")
                    || name.eq_ignore_ascii_case("transfer-encoding")
                    || name.eq_ignore_ascii_case("te")
                    || name.eq_ignore_ascii_case("trailer")
                { return None; }
                v.to_str().ok().map(|val| (name.to_string(), val.to_string()))
            })
    );

    // 确保 Content-Type 包含 application/grpc
    let has_grpc_ct = req_headers.iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v.contains("application/grpc"));
    if !has_grpc_ct {
        req_headers.push(("Content-Type".to_string(), "application/grpc".to_string()));
    }

    // 应用 proxy_set_headers
    let scheme = ctx.req().uri().scheme_str().unwrap_or("http");
    for h in &location.proxy_set_headers {
        let val = h.value
            .replace("$remote_addr", &client_ip)
            .replace("$host", upstream_host.as_ref())
            .replace("$scheme", scheme)
            .replace("$request_uri", path);
        req_headers.retain(|(k, _)| !k.eq_ignore_ascii_case(&h.name));
        req_headers.push((h.name.clone(), val));
    }

    // ── 读取请求体（gRPC 帧，全量收集）──────────────────────────────────
    let cap = ctx.req().headers()
        .get(sweety_web::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut req_body = Vec::with_capacity(cap);
    let mut body_stream = ctx.body_borrow_mut();
    while let Some(chunk) = body_stream.next().await {
        match chunk {
            Ok(b) => req_body.extend_from_slice(b.as_ref()),
            Err(_) => break,
        }
    }
    drop(body_stream);

    debug!("gRPC 转发: {} {} → {} body={}B", method, path, node.addr.as_str(), req_body.len());

    node.active_connections.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let result = crate::handler::reverse_proxy::conn::forward_request(
        &ctx.state().conn_pool,
        &node.addr, method, path, upstream_host.as_ref(),
        node.tls, &node.tls_sni, node.tls_insecure,
        &req_headers, &client_ip,
        sweety_web::body::RequestBody::from(req_body),
        false, None, None, None,
        &[], // gRPC 不做 sub_filter
        None, // gRPC 不做缓存
        scheme, // client_proto
        0, 0, 0, // keepalive_requests, keepalive_time, keepalive_max_idle（gRPC 不限制）
        10, 60, 60, // connect_timeout, read_timeout, write_timeout（gRPC 默认值）
        true,        // proxy_buffering=true（gRPC 必须完整读取响应体）
        node.send_proxy_protocol,
        Some(*ctx.req().body().socket_addr()),
    ).await;

    node.active_connections.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

    match result {
        Ok(mut resp) => {
            node.fail_count.store(0, std::sync::atomic::Ordering::Relaxed);
            // 确保 gRPC 响应有正确的 Content-Type
            let has_ct = resp.headers().contains_key(CONTENT_TYPE);
            if !has_ct {
                resp.headers_mut().insert(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/grpc"),
                );
            }
            // 注入 grpc-status=0 (OK) 如果上游没有返回
            {
                use sweety_web::http::header::HeaderName;
                let grpc_status_name = HeaderName::from_static("grpc-status");
                if !resp.headers().contains_key(&grpc_status_name) {
                    if let Ok(v) = HeaderValue::from_str("0") {
                        resp.headers_mut().insert(grpc_status_name, v);
                    }
                }
            }
            resp
        }
        Err(e) => {
            node.fail_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if node.fail_count.load(std::sync::atomic::Ordering::Relaxed) >= 3 {
                node.mark_unhealthy();
            }
            tracing::error!("gRPC 代理失败 → {}: {}", node.addr.as_str(), e);
            grpc_error(StatusCode::BAD_GATEWAY, 14, &format!("上游 {} 响应失败", node.addr.as_str()))
        }
    }
}

/// 构造 gRPC 错误响应
/// gRPC 错误格式：HTTP 200 + grpc-status 非零（或 HTTP 5xx + grpc-status）
fn grpc_error(http_status: StatusCode, grpc_status: u32, msg: &str) -> WebResponse {
    use sweety_web::http::header::HeaderName;
    let mut resp = WebResponse::new(ResponseBody::none());
    *resp.status_mut() = http_status;
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/grpc"),
    );
    if let Ok(v) = HeaderValue::from_str(itoa::Buffer::new().format(grpc_status)) {
        resp.headers_mut().insert(
            HeaderName::from_static("grpc-status"),
            v,
        );
    }
    if !msg.is_empty() {
        if let Ok(v) = HeaderValue::from_str(msg) {
            resp.headers_mut().insert(
                HeaderName::from_static("grpc-message"),
                v,
            );
        }
    }
    resp
}
