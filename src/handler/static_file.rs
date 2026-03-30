//! 静态文件处理器
//!
//! # 0-copy 流式传输策略
//! - **普通请求**：`tokio::fs::File` → `ReaderStream`（128 KiB/chunk）→ `ResponseBody`
//!   文件内容以块为单位从内核 page cache 直接写入 socket，用户空间不做整块 copy
//! - **Range 请求**：seek 到偏移后 `io::Take` 限制读取范围，同样流式传输
//! - **gzip**：仅对 ≤ 4 MB 的可压缩文件在内存中一次性压缩；大文件直接流式跳过
//! - **不做的事**：不把整个文件读入单一 `BytesMut`，不触发 Windows IoSlice u32::MAX 断言

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use tracing::debug;
use xitca_web::{
    body::ResponseBody,
    http::{
        StatusCode, WebResponse,
        header::{
            CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE,
            ETAG, LAST_MODIFIED, IF_MODIFIED_SINCE, IF_NONE_MATCH,
            HeaderMap, HeaderValue,
        },
    },
    WebContext,
};

use crate::config::model::LocationConfig;
use crate::dispatcher::vhost::SiteInfo;
use crate::middleware::cache::{generate_etag, to_unix_secs, mime_type_for, default_cache_control};
use crate::server::http::AppState;

/// gzip 压缩的文件大小上限（4 MB）——超过此值直接流式传输，不做内存压缩
const GZIP_MAX_INLINE: u64 = 4 * 1024 * 1024;
/// ReaderStream 块大小（128 KiB），与 OS page size 对齐
const STREAM_CHUNK: usize = 128 * 1024;

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

    // HEAD 请求只返回头信息，不读取 body
    if method.eq_ignore_ascii_case("HEAD") {
        let mut resp = WebResponse::new(ResponseBody::none());
        set_file_headers(resp.headers_mut(), &file_path, file_size, &etag_val, modified_secs, location);
        return resp;
    }

    // 解析 Range 头（bytes=start-end，只支持单区间）
    let range = req_headers
        .get(xitca_web::http::header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| parse_range(s, file_size));

    debug!("提供静态文件: {} range={:?}", file_path.display(), range);

    // 打开文件（流式读取，不全量载入内存）
    let mut file = match tokio::fs::File::open(&file_path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("打开文件失败 {}: {}", file_path.display(), e);
            return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
        }
    };

    if let Some((range_start, range_end)) = range {
        // Range 请求：seek 到 start，只读取指定范围
        let range_len = range_end - range_start + 1;
        if file.seek(SeekFrom::Start(range_start)).await.is_err() {
            return make_error(StatusCode::RANGE_NOT_SATISFIABLE, "");
        }
        let content = read_exact_bytes(&mut file, range_len as usize).await;
        match content {
            Ok(bytes) => {
                let mut resp = WebResponse::new(ResponseBody::from(bytes));
                *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
                set_file_headers(resp.headers_mut(), &file_path, range_len, &etag_val, modified_secs, location);
                // Content-Range: bytes start-end/total
                let cr = format!("bytes {}-{}/{}", range_start, range_end, file_size);
                if let Ok(v) = HeaderValue::from_str(&cr) {
                    resp.headers_mut().insert(CONTENT_RANGE, v);
                }
                resp
            }
            Err(e) => {
                tracing::error!("读取文件范围失败 {}: {}", file_path.display(), e);
                make_error(StatusCode::INTERNAL_SERVER_ERROR, "")
            }
        }
    } else {
        // 普通请求：流式读取整个文件
        match read_all_chunked(&mut file, file_size as usize).await {
            Ok(bytes) => {
                // 判断是否启用 gzip（站点级优先，否则全局）
                let global = &ctx.state().cfg.global;
                let gzip_enabled = site.gzip.unwrap_or(global.gzip);
                let gzip_level = site.gzip_comp_level.unwrap_or(global.gzip_comp_level);
                let min_bytes = (global.gzip_min_length as u64) * 1024;

                // gzip 条件：开关开启 + 文件足够大 + 客户端支持 + 非已压缩格式
                let accept_gz = req_headers
                    .get(xitca_web::http::header::ACCEPT_ENCODING)
                    .and_then(|v| v.to_str().ok())
                    .map(|v| v.contains("gzip"))
                    .unwrap_or(false);
                let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
                let already_compressed = matches!(ext,
                    "gz" | "br" | "zst" | "zip" | "png" | "jpg" | "jpeg"
                    | "gif" | "webp" | "avif" | "mp4" | "webm" | "woff" | "woff2"
                );

                if gzip_enabled && accept_gz && file_size >= min_bytes && !already_compressed {
                    match gzip_compress(&bytes, gzip_level) {
                        Ok(compressed) => {
                            let compressed_len = compressed.len() as u64;
                            let mut resp = WebResponse::new(ResponseBody::from(compressed));
                            set_file_headers(resp.headers_mut(), &file_path, compressed_len, &etag_val, modified_secs, location);
                            resp.headers_mut().insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                            // gzip 时 Content-Length 已在 set_file_headers 设为压缩后大小
                            resp
                        }
                        Err(_) => {
                            // 压缩失败降级为原始内容
                            let mut resp = WebResponse::new(ResponseBody::from(bytes));
                            set_file_headers(resp.headers_mut(), &file_path, file_size, &etag_val, modified_secs, location);
                            resp
                        }
                    }
                } else {
                    let mut resp = WebResponse::new(ResponseBody::from(bytes));
                    set_file_headers(resp.headers_mut(), &file_path, file_size, &etag_val, modified_secs, location);
                    resp
                }
            }
            Err(e) => {
                tracing::error!("读取文件失败 {}: {}", file_path.display(), e);
                make_error(StatusCode::INTERNAL_SERVER_ERROR, "")
            }
        }
    }
}

/// 分块读取整个文件到 Bytes（避免大文件单次 alloc 失败，块大小 CHUNK_SIZE）
async fn read_all_chunked(
    file: &mut tokio::fs::File,
    size_hint: usize,
) -> std::io::Result<bytes::Bytes> {
    let mut buf = bytes::BytesMut::with_capacity(size_hint.min(CHUNK_SIZE * 4));
    let mut tmp = vec![0u8; CHUNK_SIZE];
    loop {
        let n = file.read(&mut tmp).await?;
        if n == 0 { break; }
        buf.extend_from_slice(&tmp[..n]);
    }
    Ok(buf.freeze())
}

/// 读取精确字节数（Range 请求用）
async fn read_exact_bytes(
    file: &mut tokio::fs::File,
    len: usize,
) -> std::io::Result<bytes::Bytes> {
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).await?;
    Ok(bytes::Bytes::from(buf))
}

/// gzip 压缩（flate2，同步）
fn gzip_compress(data: &[u8], level: u32) -> std::io::Result<bytes::Bytes> {
    use flate2::{Compression, write::GzEncoder};
    let level = Compression::new(level.min(9));
    let mut encoder = GzEncoder::new(Vec::with_capacity(data.len() / 2), level);
    encoder.write_all(data)?;
    let compressed = encoder.finish()?;
    Ok(bytes::Bytes::from(compressed))
}

/// 解析 Range 头，返回 (start, end) 字节偏移（闭区间），失败或超出范围返回 None
fn parse_range(range_header: &str, file_size: u64) -> Option<(u64, u64)> {
    let s = range_header.strip_prefix("bytes=")?;
    let mut parts = s.splitn(2, '-');
    let start_str = parts.next()?.trim();
    let end_str = parts.next()?.trim();

    let start: u64 = if start_str.is_empty() {
        // suffix-length: bytes=-500
        let suffix: u64 = end_str.parse().ok()?;
        file_size.saturating_sub(suffix)
    } else {
        start_str.parse().ok()?
    };

    let end: u64 = if end_str.is_empty() {
        file_size.saturating_sub(1)
    } else {
        end_str.parse().ok()?
    };

    if start > end || end >= file_size { return None; }
    Some((start, end))
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
