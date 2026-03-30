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

/// 构建内置默认错误 HTML 页面（供所有模块调用）
pub fn build_default_html(code: u16) -> String {
    let text = status_text(code);
    let description = error_description(code);
    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>{code} {text}</title>
  <style>
    body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
           display: flex; align-items: center; justify-content: center;
           min-height: 100vh; margin: 0; background: #f5f5f5; }}
    .container {{ text-align: center; padding: 40px; background: white;
                  border-radius: 12px; box-shadow: 0 2px 20px rgba(0,0,0,0.1); }}
    h1 {{ font-size: 6rem; margin: 0; color: #333; font-weight: 800; }}
    h2 {{ font-size: 1.5rem; color: #666; margin: 10px 0; }}
    p  {{ color: #999; margin: 20px 0 0; font-size: 0.9rem; }}
    hr {{ border: none; border-top: 1px solid #eee; margin: 20px 0; }}
  </style>
</head>
<body>
  <div class="container">
    <h1>{code}</h1>
    <h2>{text}</h2>
    <hr>
    <p>{description}</p>
    <p style="font-size:0.75rem;color:#ccc">Sweety Web Server</p>
  </div>
</body>
</html>"#
    )
}

/// HTTP 状态码标准文本
fn status_text(code: u16) -> &'static str {
    match code {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}

/// 错误描述文字（展示在错误页面中）
fn error_description(code: u16) -> &'static str {
    match code {
        400 => "请求格式有误，服务器无法理解该请求。",
        401 => "访问此资源需要身份验证。",
        403 => "您没有权限访问此资源。",
        404 => "您请求的页面不存在或已被移除。",
        405 => "请求方法不被允许。",
        408 => "请求超时，请重试。",
        413 => "请求体积过大。",
        429 => "请求过于频繁，请稍后再试。",
        500 => "服务器内部发生错误，请联系管理员。",
        502 => "上游服务器返回无效响应。",
        503 => "服务暂时不可用，请稍后重试。",
        504 => "等待上游服务器响应超时。",
        _ => "发生了一个错误。",
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
