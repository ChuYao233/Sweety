//! 响应处理模块
//! 负责：响应头透传（跳过 hop-by-hop）、Set-Cookie 改写、Location 重写、proxy_error

use sweety_web::{
    body::ResponseBody,
    http::{
        StatusCode, WebResponse,
        header::{CONTENT_TYPE, HeaderValue},
    },
};

/// 编译期 hop-by-hop 头完美哈希表（全小写 key，O(1) 查找）
static HOP_BY_HOP_SET: phf::Set<&'static str> = phf::phf_set! {
    "connection", "keep-alive", "proxy-authenticate", "proxy-authorization",
    "te", "trailer", "transfer-encoding", "upgrade", "content-length",
    "host", "proxy-connection",
};

/// 检查头名是否为 hop-by-hop（大小写不敏感，用栈 buffer 转小写）
#[inline]
pub fn is_hop_by_hop(name: &str) -> bool {
    let mut buf = [0u8; 32];
    let b = name.as_bytes();
    if b.len() > 32 { return false; }
    for (i, &c) in b.iter().enumerate() { buf[i] = c.to_ascii_lowercase(); }
    let lower = unsafe { std::str::from_utf8_unchecked(&buf[..b.len()]) };
    HOP_BY_HOP_SET.contains(lower)
}

/// 对响应体内容做 sub_filter 替换（等价 Nginx sub_filter）
///
/// - pattern 以 `~` 开头：按正则替换（支持 $1 $2 捕获组）
/// - 否则：纯字符串替换
/// - 仅对文本类 Content-Type（html/json/js/text）生效
pub fn apply_sub_filter(
    body: Vec<u8>,
    headers: &[(String, String)],
    filters: &[crate::config::model::SubFilter],
) -> Vec<u8> {
    if filters.is_empty() { return body; }

    // 只处理文本类响应
    let is_text = headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("content-type") && (
            v.contains("html") || v.contains("json") ||
            v.contains("javascript") || v.contains("text")
        )
    });
    if !is_text { return body; }

    let Ok(text) = std::str::from_utf8(&body) else { return body; };
    let mut result = text.to_string();

    for f in filters {
        if let Some(pattern) = f.pattern.strip_prefix('~') {
            // 正则替换：用全局缓存避免每请求重建
            if let Some(re) = sub_filter_regex(pattern.trim()) {
                result = re.replace_all(&result, f.replacement.as_str()).into_owned();
            }
        } else {
            // 字符串替换（全量）
            result = result.replace(f.pattern.as_str(), &f.replacement);
        }
    }

    result.into_bytes()
}

/// 全局 sub_filter 正则缓存（pattern → 预编译 Regex），避免每请求 Regex::new
static SUB_FILTER_RE_CACHE: std::sync::OnceLock<dashmap::DashMap<String, regex::Regex>> =
    std::sync::OnceLock::new();

fn sub_filter_regex(pattern: &str) -> Option<regex::Regex> {
    let map = SUB_FILTER_RE_CACHE.get_or_init(dashmap::DashMap::new);
    if let Some(re) = map.get(pattern) {
        return Some(re.clone());
    }
    match regex::Regex::new(pattern) {
        Ok(re) => { map.insert(pattern.to_string(), re.clone()); Some(re) }
        Err(_) => None,
    }
}

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
    use sweety_web::http::header::HeaderName;

    for (k, v) in headers {
        // hop-by-hop 头不透传给客户端，phf O(1) 查找
        if is_hop_by_hop(k) { continue; }

        // Location：将上游 URL 前缀替换为客户端 URL（等价 Nginx proxy_redirect）
        if k.eq_ignore_ascii_case("location") {
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
        let is_set_cookie = k.eq_ignore_ascii_case("set-cookie");
        let val_str = if is_set_cookie && (strip_cookie_secure || proxy_cookie_domain.is_some()) {
            rewrite_set_cookie(v, strip_cookie_secure, proxy_cookie_domain)
        } else {
            v.clone()
        };

        if let Ok(name) = HeaderName::from_bytes(k.as_bytes()) {
            if is_set_cookie {
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
            if strip_secure && trimmed.eq_ignore_ascii_case("secure") {
                return None; // 去掉 Secure 标志
            }
            if let Some(domain) = new_domain {
                // 小写不敏感匹配 "domain="
                if trimmed.len() > 7 && trimmed[..7].eq_ignore_ascii_case("domain=") {
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
