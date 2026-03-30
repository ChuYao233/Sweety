//! 反向代理处理器
//! 支持：HTTP / HTTPS / WS / WSS + gzip / chunked 透传
//! 子模块职责：
//!   lb.rs         — 负载均衡、节点状态、健康检查
//!   tls_client.rs — TLS 客户端连接
//!   conn.rs       — HTTP 连接层（发请求/读响应）
//!   response.rs   — 响应头透传、Cookie/Location 改写

pub mod conn;
pub mod lb;
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
    let client_headers: Vec<(String, String)> = ctx.req().headers()
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

    // ── 检测 WebSocket 升级请求 ───────────────────────────────────────────
    let is_ws = ctx.req().headers()
        .get(xitca_web::http::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_lowercase().contains("websocket"))
        .unwrap_or(false);

    // ── 读取请求体（POST/PUT 等） ─────────────────────────────────────────
    let content_length = ctx.req().headers()
        .get(xitca_web::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);

    let request_body: Vec<u8> = if content_length > 0 {
        let mut bytes = Vec::with_capacity(content_length);
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

    // ── 转发请求 ───────────────────────────────
    node.active_connections.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // WS/WSS 请求走专用的零拷贝反代路径
    let resp = if is_ws {
        ws_proxy::handle_ws_proxy(
            ctx, &node.addr, node.tls, &node.tls_sni, node.tls_insecure,
            &client_headers, &client_ip_str, &upstream_host, &path,
            strip_cookie_secure, proxy_cookie_domain.as_deref(),
            proxy_redirect_from.as_deref(), proxy_redirect_to.as_deref(),
        ).await
    } else {
        let result = conn::forward_request(
            &node.addr, &method, &path, &upstream_host,
            node.tls, &node.tls_sni, node.tls_insecure,
            &client_headers, &client_ip_str,
            &request_body, is_ws,
            strip_cookie_secure, proxy_cookie_domain.as_deref(),
            proxy_redirect_from.as_deref(), proxy_redirect_to.as_deref(),
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
    resp
}
