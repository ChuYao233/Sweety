//! 错误页面处理器
//! 负责：根据状态码生成内置 HTML 错误页面，支持自定义错误页文件

use std::path::PathBuf;
use std::collections::HashMap;

/// 错误页面配置（每站点维护一份）
#[derive(Debug, Clone, Default)]
pub struct ErrorPageConfig {
    /// 状态码 → 自定义页面文件路径
    pub pages: HashMap<u16, PathBuf>,
}

/// 根据状态码加载错误页 HTML（优先自定义文件，回退内置默认页）
pub async fn load_error_page(code: u16, cfg: Option<&ErrorPageConfig>) -> String {
    if let Some(error_cfg) = cfg {
        if let Some(page_path) = error_cfg.pages.get(&code) {
            if let Ok(content) = tokio::fs::read_to_string(page_path).await {
                return content;
            }
        }
    }
    build_default_html(code)
}

/// 构建内置默认错误 HTML 页面（中英双语）
pub fn build_default_html(code: u16) -> String {
    let (text, desc_zh, desc_en) = status_info(code);
    let color = if code >= 500 { "#e74c3c" } else if code >= 400 { "#e67e22" } else { "#3498db" };
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>{code} {text}</title>
  <style>
    body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif;
           display: flex; align-items: center; justify-content: center;
           min-height: 100vh; margin: 0; background: #f0f2f5; }}
    .box {{ text-align: center; padding: 48px 56px; background: #fff;
            border-radius: 16px; box-shadow: 0 4px 32px rgba(0,0,0,.08); max-width: 480px; }}
    .code {{ font-size: 5.5rem; font-weight: 900; color: {color}; line-height: 1; margin: 0 0 8px; }}
    .title {{ font-size: 1.4rem; font-weight: 600; color: #333; margin: 0 0 20px; }}
    hr {{ border: none; border-top: 1px solid #eee; margin: 0 0 20px; }}
    .zh {{ color: #555; font-size: .95rem; margin: 0 0 8px; }}
    .en {{ color: #999; font-size: .85rem; margin: 0 0 24px; }}
    .foot {{ font-size: .72rem; color: #bbb; margin: 0; }}
  </style>
</head>
<body>
  <div class="box">
    <div class="code">{code}</div>
    <div class="title">{text}</div>
    <hr>
    <p class="zh">{desc_zh}</p>
    <p class="en">{desc_en}</p>
    <p class="foot">Sweety Web Server</p>
  </div>
</body>
</html>"#
    )
}

/// 返回 (English status, 中文描述, English description)
fn status_info(code: u16) -> (&'static str, &'static str, &'static str) {
    match code {
        // 4xx 客户端错误
        400 => ("Bad Request",
                "请求格式有误，服务器无法解析该请求。",
                "The server could not understand the request due to invalid syntax."),
        401 => ("Unauthorized",
                "访问此资源需要身份验证，请登录后重试。",
                "Authentication is required. Please log in and try again."),
        403 => ("Forbidden",
                "您没有权限访问此资源。",
                "You don't have permission to access this resource."),
        404 => ("Not Found",
                "您请求的页面不存在或已被移除。",
                "The page you requested could not be found."),
        405 => ("Method Not Allowed",
                "不支持该请求方法。",
                "The request method is not supported for this resource."),
        408 => ("Request Timeout",
                "请求超时，请检查网络后重试。",
                "The server timed out waiting for the request."),
        413 => ("Payload Too Large",
                "请求体过大，超出服务器限制。",
                "The request body exceeds the server's size limit."),
        418 => ("I'm a Teapot",
                "服务器是一个茶壶，拒绝冲咖啡。",
                "The server refuses to brew coffee because it is, permanently, a teapot."),
        421 => ("Misdirected Request",
                "请求被发到了无法响应的服务器，请检查域名配置。",
                "The request was directed at a server that is not able to produce a response."),
        429 => ("Too Many Requests",
                "请求过于频繁，请稍后再试。",
                "Too many requests in a given amount of time. Please slow down."),
        // 5xx 服务器错误
        500 => ("Internal Server Error",
                "服务器内部发生错误，请稍后重试或联系管理员。",
                "An unexpected error occurred on the server. Please try again later."),
        502 => ("Bad Gateway",
                "上游服务器返回了无效响应。",
                "The upstream server received an invalid response."),
        503 => ("Service Unavailable",
                "服务暂时不可用，可能正在维护中，请稍后重试。",
                "The service is temporarily unavailable. Please try again later."),
        504 => ("Gateway Timeout",
                "等待上游服务器响应超时。",
                "The upstream server did not respond in time."),
        _ => ("Error",
              "发生了一个未知错误。",
              "An unexpected error occurred."),
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_404_default_html() {
        let html = build_default_html(404);
        assert!(html.contains("404"));
        assert!(html.contains("Not Found"));
    }

    #[test]
    fn test_502_default_html() {
        let html = build_default_html(502);
        assert!(html.contains("502"));
        assert!(html.contains("Bad Gateway"));
    }

    #[tokio::test]
    async fn test_load_error_page_fallback() {
        // 自定义页面文件不存在时，应回退到默认页
        let mut cfg = ErrorPageConfig::default();
        cfg.pages.insert(404, PathBuf::from("/nonexistent/404.html"));
        let html = load_error_page(404, Some(&cfg)).await;
        assert!(html.contains("404"));
    }

    #[tokio::test]
    async fn test_load_custom_error_page() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        write!(f, "<h1>Custom 404</h1>").unwrap();
        let mut cfg = ErrorPageConfig::default();
        cfg.pages.insert(404, f.path().to_path_buf());
        let html = load_error_page(404, Some(&cfg)).await;
        assert_eq!(html, "<h1>Custom 404</h1>");
    }
}
