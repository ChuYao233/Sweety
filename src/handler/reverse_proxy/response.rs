//! 响应处理模块
//! 负责：响应头透传（跳过 hop-by-hop）、Set-Cookie 改写、Location 重写、proxy_error

use xitca_web::{
    body::ResponseBody,
    http::{
        StatusCode, WebResponse,
        header::{CONTENT_TYPE, HeaderValue},
    },
};

/// 将上游响应头全量透传给客户端（Nginx proxy_pass 默认行为）
///
/// - 跳过 hop-by-hop 头（Connection / Transfer-Encoding 等）
/// - `strip_cookie_secure`：去掉 Set-Cookie 的 Secure 标志（HTTP 代理 HTTPS 上游时使用）
/// - `proxy_cookie_domain`：替换 Set-Cookie 的 Domain 属性
/// - `proxy_redirect_from/to`：重写 Location 头中的上游 URL
pub fn apply_response_headers(
    resp: &mut WebResponse,
    headers: &[(String, String)],
    strip_cookie_secure: bool,
    proxy_cookie_domain: Option<&str>,
    proxy_redirect_from: Option<&str>,
    proxy_redirect_to: Option<&str>,
) {
    use xitca_web::http::header::HeaderName;

    // Hop-by-hop 头不传给客户端；content-length 由 Sweety 根据实际 body 重算
    const HOP_BY_HOP: &[&str] = &[
        "connection", "keep-alive", "proxy-authenticate", "proxy-authorization",
        "te", "trailer", "transfer-encoding", "upgrade", "content-length",
    ];

    for (k, v) in headers {
        let kl = k.to_lowercase();
        if HOP_BY_HOP.contains(&kl.as_str()) { continue; }

        // Location：将上游 URL 前缀替换为客户端 URL（等价 Nginx proxy_redirect）
        if kl == "location" {
            if let (Some(from), Some(to)) = (proxy_redirect_from, proxy_redirect_to) {
                let rewritten = v.replacen(from, to, 1);
                tracing::info!("重定向 Location: {} → {}", v, rewritten);
                if let (Ok(name), Ok(val)) = (
                    HeaderName::from_bytes(k.as_bytes()),
                    HeaderValue::from_str(&rewritten),
                ) {
                    resp.headers_mut().insert(name, val);
                }
                continue;
            }
        }

        // Set-Cookie：去 Secure、替换 Domain
        let val_str = if kl == "set-cookie" && (strip_cookie_secure || proxy_cookie_domain.is_some()) {
            rewrite_set_cookie(v, strip_cookie_secure, proxy_cookie_domain)
        } else {
            v.clone()
        };

        if let Ok(name) = HeaderName::from_bytes(k.as_bytes()) {
            if kl == "set-cookie" {
                // Set-Cookie 可有多个，用 append
                if let Ok(val) = HeaderValue::from_str(&val_str) {
                    resp.headers_mut().append(name, val);
                }
            } else if let Ok(val) = HeaderValue::from_str(&val_str) {
                resp.headers_mut().insert(name, val);
            }
        }
    }
}

/// 修改 Set-Cookie 属性：
/// - 去掉 `Secure` 标志（HTTP 代理 HTTPS 上游时必须，否则浏览器不存 Cookie）
/// - 替换 `Domain=upstream` 为指定值
fn rewrite_set_cookie(cookie: &str, strip_secure: bool, new_domain: Option<&str>) -> String {
    cookie.split(';')
        .filter_map(|part| {
            let trimmed = part.trim();
            let lower = trimmed.to_lowercase();

            if strip_secure && lower == "secure" {
                return None; // 去掉 Secure 标志
            }
            if let Some(domain) = new_domain {
                if lower.starts_with("domain=") {
                    return Some(format!("Domain={}", domain));
                }
            }
            Some(trimmed.to_string())
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// 解析 HTTP 状态行中的状态码（如 "HTTP/1.1 200 OK" → 200）
pub fn parse_status_code(status_line: &str) -> u16 {
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(502)
}

/// 构造 HTML 错误响应
pub fn proxy_error(status: StatusCode, _msg: &str) -> WebResponse {
    let body = crate::handler::error_page::build_default_html(status.as_u16());
    let mut resp = WebResponse::new(ResponseBody::from(body));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp
}
