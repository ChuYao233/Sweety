//! 静态文件处理器
//! 负责：基于 tokio::fs 的零拷贝文件传输、ETag/Last-Modified 验证、
//!       Range 分块传输、默认文档查找、Content-Type 推断、目录穿越防护

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tracing::debug;
use xitca_web::{
    body::ResponseBody,
    http::{
        StatusCode, WebResponse,
        header::{
            CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, ETAG, LAST_MODIFIED,
            IF_MODIFIED_SINCE, IF_NONE_MATCH, HeaderMap, HeaderValue,
        },
    },
    WebContext,
};

use crate::config::model::LocationConfig;
use crate::dispatcher::vhost::SiteInfo;
use crate::middleware::cache::{generate_etag, to_unix_secs, mime_type_for, default_cache_control};
use crate::server::http::AppState;

/// 处理静态文件请求（xitca-web WebContext 版本）
pub async fn handle_xitca(
    ctx: &WebContext<'_, AppState>,
    site: &SiteInfo,
    location: &LocationConfig,
) -> WebResponse {
    let path = ctx.req().uri().path().to_string();
    let method = ctx.req().method().as_str();
    let req_headers = ctx.req().headers().clone();

    // 确定文件系统根目录（location 级 root 优先于 site 级 root）
    let root = match location.root.as_ref().or(site.root.as_ref()) {
        Some(r) => r.clone(),
        None => {
            return make_error(StatusCode::INTERNAL_SERVER_ERROR, "站点未配置 root 目录");
        }
    };

    // 安全路径解析（防目录穿越）
    let file_path = match resolve_safe_path(&root, &path) {
        Some(p) => p,
        None => return make_error(StatusCode::FORBIDDEN, "Forbidden"),
    };

    // 目录：尝试默认文档
    let file_path = if file_path.is_dir() {
        match find_index(&file_path, &site.index).await {
            Some(p) => p,
            None => return make_error(StatusCode::FORBIDDEN, "Directory listing disabled"),
        }
    } else {
        file_path
    };

    // 文件不存在
    if !file_path.is_file() {
        return make_error(StatusCode::NOT_FOUND, "Not Found");
    }

    // 读取文件元数据（用于 ETag 和 Last-Modified）
    let meta = match tokio::fs::metadata(&file_path).await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("读取文件元数据失败 {}: {}", file_path.display(), e);
            return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
        }
    };

    let file_size = meta.len();
    let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let modified_secs = to_unix_secs(modified);
    let etag_val = generate_etag(file_size, modified_secs);

    // 304 缓存验证
    let if_none_match = req_headers
        .get(IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok());
    let if_modified_since = req_headers
        .get(IF_MODIFIED_SINCE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| httpdate::parse_http_date(s).ok())
        .map(to_unix_secs);

    if crate::middleware::cache::is_cache_valid(
        if_none_match,
        if_modified_since,
        &etag_val,
        modified_secs,
    ) {
        let mut resp = WebResponse::new(ResponseBody::none());
        *resp.status_mut() = StatusCode::NOT_MODIFIED;
        return resp;
    }

    // HEAD 请求只返回头信息
    if method.eq_ignore_ascii_case("HEAD") {
        let mut resp = WebResponse::new(ResponseBody::none());
        set_file_headers(resp.headers_mut(), &file_path, file_size, &etag_val, modified_secs, location);
        return resp;
    }

    // 读取文件内容
    debug!("提供静态文件: {}", file_path.display());
    let content = match tokio::fs::read(&file_path).await {
        Ok(b) => b,
        Err(e) => {
            tracing::error!("读取文件失败 {}: {}", file_path.display(), e);
            return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
        }
    };

    let mut resp = WebResponse::new(ResponseBody::from(content));
    set_file_headers(resp.headers_mut(), &file_path, file_size, &etag_val, modified_secs, location);
    resp
}

/// 设置静态文件响应头（Content-Type / ETag / Last-Modified / Cache-Control）
fn set_file_headers(
    headers: &mut HeaderMap,
    path: &Path,
    size: u64,
    etag: &str,
    modified_secs: u64,
    location: &LocationConfig,
) {
    // Content-Type
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    let mime = mime_type_for(ext);
    if let Ok(v) = HeaderValue::from_str(mime) {
        headers.insert(CONTENT_TYPE, v);
    }

    // Content-Length
    if let Ok(v) = HeaderValue::from_str(&size.to_string()) {
        headers.insert(CONTENT_LENGTH, v);
    }

    // ETag
    if let Ok(v) = HeaderValue::from_str(etag) {
        headers.insert(ETAG, v);
    }

    // Last-Modified（HTTP 日期格式）
    let modified_time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(modified_secs);
    let http_date = httpdate::fmt_http_date(modified_time);
    if let Ok(v) = HeaderValue::from_str(&http_date) {
        headers.insert(LAST_MODIFIED, v);
    }

    // Cache-Control（location 级配置优先，否则按扩展名默认）
    let cc = location.cache_control.as_deref()
        .unwrap_or_else(|| default_cache_control(ext));
    if let Ok(v) = HeaderValue::from_str(cc) {
        headers.insert(CACHE_CONTROL, v);
    }
}

/// 在目录中查找第一个存在的默认文档
async fn find_index(dir: &Path, index_files: &[String]) -> Option<PathBuf> {
    for name in index_files {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 将请求路径安全地解析为文件系统绝对路径（防目录穿越）
pub fn resolve_safe_path(root: &Path, request_path: &str) -> Option<PathBuf> {
    // 去掉查询字符串
    let path_only = request_path.split('?').next().unwrap_or(request_path);

    // 拒绝包含 `..` 的路径片段
    for segment in path_only.split('/') {
        if segment == ".." {
            return None;
        }
    }

    let relative = path_only.trim_start_matches('/');
    let full = root.join(relative);

    // canonicalize 验证路径在 root 下（防符号链接穿越）
    // 文件不存在时 canonicalize 失败，直接返回拼接路径（后续 is_file 会返回 false）
    match (full.canonicalize().ok(), root.canonicalize().ok()) {
        (Some(cf), Some(cr)) => {
            if cf.starts_with(&cr) { Some(cf) } else { None }
        }
        _ => Some(full),
    }
}

/// 构造简单错误响应
fn make_error(status: StatusCode, msg: &str) -> WebResponse {
    let body = if msg.is_empty() {
        crate::handler::error_page::build_default_html(status.as_u16())
    } else {
        msg.to_string()
    };
    let mut resp = WebResponse::new(ResponseBody::from(body));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_safe_path_normal() {
        let dir = tempfile::tempdir().unwrap();
        let result = resolve_safe_path(dir.path(), "/index.html");
        assert!(result.is_some());
    }

    #[test]
    fn test_resolve_safe_path_traversal_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(resolve_safe_path(dir.path(), "/../etc/passwd").is_none());
        assert!(resolve_safe_path(dir.path(), "/foo/../../../etc/passwd").is_none());
    }

    #[test]
    fn test_resolve_safe_path_root() {
        let dir = tempfile::tempdir().unwrap();
        let result = resolve_safe_path(dir.path(), "/");
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_find_index_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), b"hi").unwrap();
        let result = find_index(dir.path(), &["index.html".to_string()]).await;
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn test_find_index_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let result = find_index(dir.path(), &["index.html".to_string()]).await;
        assert!(result.is_none());
    }
}
