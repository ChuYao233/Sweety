//! 路由分发层
//! 负责：虚拟主机选择 → Location 路径匹配 → Rewrite 规则应用 → Handler 调用

pub mod location;
pub mod rewrite;
pub mod vhost;

use std::net::SocketAddr;

use crate::dispatcher::vhost::VHostRegistry;

/// 分发结果（简化的 HTTP 响应表示，后续版本替换为完整 Response 类型）
pub struct DispatchResponse {
    pub status_code: u16,
    pub status_text: &'static str,
    pub body: String,
}

/// 请求分发入口
///
/// 根据 Host → Location → Rewrite → Handler 依次处理请求，
/// 返回最终响应
pub async fn dispatch(
    registry: &VHostRegistry,
    host: &str,
    method: &str,
    path: &str,
    peer_addr: SocketAddr,
) -> DispatchResponse {
    // 1. 虚拟主机匹配
    let site = match registry.lookup(host) {
        Some(s) => s,
        None => {
            return DispatchResponse {
                status_code: 404,
                status_text: "Not Found",
                body: format!("No virtual host configured for: {}", host),
            }
        }
    };

    // 2. Rewrite 规则应用
    let rewritten_path = rewrite::apply_rewrites(&site.rewrites, path);
    let effective_path = rewritten_path.as_deref().unwrap_or(path);

    // 3. Location 匹配
    let location = match location::match_location(&site.locations, effective_path) {
        Some(loc) => loc,
        None => {
            return DispatchResponse {
                status_code: 404,
                status_text: "Not Found",
                body: format!("No location matched: {}", effective_path),
            }
        }
    };

    // 4. 直接返回状态码（健康检查等）
    if let Some(code) = location.return_code {
        let text = status_text(code);
        return DispatchResponse {
            status_code: code,
            status_text: text,
            body: String::new(),
        };
    }

    // 5. 根据 handler 类型分发
    use crate::config::model::HandlerType;
    match location.handler {
        HandlerType::Static => {
            crate::handler::static_file::handle(site, location, method, effective_path).await
        }
        HandlerType::Fastcgi => {
            crate::handler::fastcgi::handle(site, location, method, effective_path, peer_addr)
                .await
        }
        HandlerType::Websocket => crate::handler::websocket::handle_upgrade_check(location).await,
        HandlerType::ReverseProxy => {
            crate::handler::reverse_proxy::handle(site, location, method, effective_path).await
        }
    }
}

/// HTTP 状态码对应的标准文本
fn status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}
