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

use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::debug;
use xitca_web::{
    body::ResponseBody,
    http::{
        StatusCode, WebResponse,
        header::{
            ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE,
            CONTENT_TYPE, ETAG, LAST_MODIFIED, IF_MODIFIED_SINCE, IF_NONE_MATCH,
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
/// ReaderStream 块大小（32 KiB）
/// 平衡吞吐量和内存占用：64 线程 × 32KiB = 2MiB，远小于 128KiB 时的 8MiB
const STREAM_CHUNK: usize = 32 * 1024;

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

    // 文件不存在时应用 try_files（等价 Nginx try_files $uri $uri/ /index.html）
    let file_path = if !file_path.is_file() {
        if !location.try_files.is_empty() {
            match try_files_resolve_inner(&root, &path, &location.try_files, &site.index).await {
                TryFilesResult::File(p) => p,
                TryFilesResult::Code(code) => {
                    return make_error(StatusCode::from_u16(code).unwrap_or(StatusCode::NOT_FOUND), "");
                }
                TryFilesResult::NotFound => {
                    return make_error(StatusCode::NOT_FOUND, "Not Found");
                }
            }
        } else {
            return make_error(StatusCode::NOT_FOUND, "Not Found");
        }
    } else {
        file_path
    };

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

    debug!("提供静态文件: {} size={} range={:?}", file_path.display(), file_size, range);

    // 打开文件
    let mut file = match tokio::fs::File::open(&file_path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("打开文件失败 {}: {}", file_path.display(), e);
            return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
        }
    };

    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

    if let Some((range_start, range_end)) = range {
        // ── Range 请求：seek + Take 限制范围，流式传输 ──────────────────
        let range_len = range_end - range_start + 1;
        if file.seek(SeekFrom::Start(range_start)).await.is_err() {
            return make_error(StatusCode::RANGE_NOT_SATISFIABLE, "");
        }
        // FileStream 用 remaining 参数限制读取字节数，不分配额外堆内存
        let stream = FileStream::new(file, range_len);
        let body = ResponseBody::box_stream(stream);
        let mut resp = WebResponse::new(body);
        *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
        set_file_headers(resp.headers_mut(), &file_path, range_len, &etag_val, modified_secs, location);
        let cr = format!("bytes {}-{}/{}", range_start, range_end, file_size);
        if let Ok(v) = HeaderValue::from_str(&cr) {
            resp.headers_mut().insert(CONTENT_RANGE, v);
        }
        resp
    } else {
        // ── 普通请求 ────────────────────────────────────────────────────
        let global = &ctx.state().cfg.global;
        let gzip_enabled = site.gzip.unwrap_or(global.gzip);
        let gzip_level = site.gzip_comp_level.unwrap_or(global.gzip_comp_level);
        let min_bytes = (global.gzip_min_length as u64) * 1024;
        let accept_gz = req_headers
            .get(ACCEPT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.contains("gzip"))
            .unwrap_or(false);
        let already_compressed = matches!(ext,
            "gz" | "br" | "zst" | "zip" | "png" | "jpg" | "jpeg"
            | "gif" | "webp" | "avif" | "mp4" | "webm" | "woff" | "woff2"
        );

        // gzip：仅对 ≤ GZIP_MAX_INLINE 的小文件做内存压缩，大文件直接流式
        if gzip_enabled && accept_gz && !already_compressed
            && file_size >= min_bytes && file_size <= GZIP_MAX_INLINE
        {
            // 小文件：一次读入内存压缩
            let mut raw = Vec::with_capacity(file_size as usize);
            if let Err(e) = file.read_to_end(&mut raw).await {
                tracing::error!("读取文件失败 {}: {}", file_path.display(), e);
                return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
            }
            // 压缩后立即 drop raw，避免 raw + compressed 同时占用双份内存
            let compress_result = gzip_compress(&raw, gzip_level);
            drop(raw);
            match compress_result {
                Ok(compressed) => {
                    let clen = compressed.len() as u64;
                    let mut resp = WebResponse::new(ResponseBody::from(compressed));
                    set_file_headers(resp.headers_mut(), &file_path, clen, &etag_val, modified_secs, location);
                    resp.headers_mut().insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                    resp
                }
                Err(_) => {
                    // 压缩失败，重新打开文件流式传输（file 已被消耗）
                    match tokio::fs::File::open(&file_path).await {
                        Ok(f2) => stream_file_response(f2, &file_path, file_size, &etag_val, modified_secs, location),
                        Err(_) => make_error(StatusCode::INTERNAL_SERVER_ERROR, ""),
                    }
                }
            }
        } else {
            // 大文件 / 不压缩：ReaderStream 流式传输，0 内存 copy
            stream_file_response(file, &file_path, file_size, &etag_val, modified_secs, location)
        }
    }
}

/// 把打开的文件包装为流式 ResponseBody
///
/// 使用预分配的固定 `BytesMut` 缓冲区，每次 poll 复用同一块内存（`split_to` 切出已读部分），
/// 避免 `ReaderStream` 每次 poll 新建 32KiB 堆内存的高频 alloc/free 开销。
/// 连接断开时 Future 被 drop，`tokio::fs::File` 句柄立即释放。
fn stream_file_response(
    file: tokio::fs::File,
    file_path: &Path,
    file_size: u64,
    etag_val: &str,
    modified_secs: u64,
    location: &LocationConfig,
) -> WebResponse {
    let stream = FileStream::new(file, file_size);
    let body = ResponseBody::box_stream(stream);
    let mut resp = WebResponse::new(body);
    set_file_headers(resp.headers_mut(), file_path, file_size, etag_val, modified_secs, location);
    resp
}

/// 高性能文件流
///
/// # 设计要点
/// - 读缓冲为**栈上固定数组**（STREAM_CHUNK 字节），不在堆上积累
/// - 每次 poll 仅分配一次精确大小的 `Bytes`（copy_from_slice），发送后立即释放
/// - 每个并发连接内存占用 = STREAM_CHUNK（栈）+ 当前 chunk（堆，发完即释放）
/// - 连接断开/超时时 Future 被 drop，`File` fd 立即归还 OS
/// - 无 unsafe，无 GC 压力，无内存碎片
struct FileStream {
    file:      tokio::fs::File,
    remaining: u64,
}

impl FileStream {
    fn new(file: tokio::fs::File, size: u64) -> Self {
        Self { file, remaining: size }
    }
}

impl futures_util::Stream for FileStream {
    type Item = std::io::Result<bytes::Bytes>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        use tokio::io::AsyncRead;

        if self.remaining == 0 {
            return Poll::Ready(None);
        }

        // 栈上读缓冲：STREAM_CHUNK 字节，不产生堆分配
        let read_size = (STREAM_CHUNK as u64).min(self.remaining) as usize;
        let mut stack_buf = [0u8; STREAM_CHUNK];
        let mut read_buf = tokio::io::ReadBuf::new(&mut stack_buf[..read_size]);

        let me = self.get_mut();
        match std::pin::Pin::new(&mut me.file).poll_read(cx, &mut read_buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Ready(Ok(())) => {
                let filled = read_buf.filled().len();
                if filled == 0 {
                    Poll::Ready(None)
                } else {
                    me.remaining = me.remaining.saturating_sub(filled as u64);
                    // copy_from_slice：一次精确大小的堆分配，发出后计数清零即释放
                    Poll::Ready(Some(Ok(bytes::Bytes::copy_from_slice(&stack_buf[..filled]))))
                }
            }
        }
    }
}

/// gzip 压缩（flate2，仅用于小文件）
fn gzip_compress(data: &[u8], level: u32) -> std::io::Result<bytes::Bytes> {
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    let mut encoder = GzEncoder::new(
        Vec::with_capacity(data.len() / 2),
        Compression::new(level.min(9)),
    );
    encoder.write_all(data)?;
    Ok(bytes::Bytes::from(encoder.finish()?))
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

/// try_files 解析结果
pub enum TryFilesResult {
    /// 找到可用文件（静态文件或 PHP 脚本，调用方根据扩展名分流）
    File(PathBuf),
    /// 返回指定错误码（如 =404）
    Code(u16),
    /// 所有路径均不存在
    NotFound,
}

/// 按 try_files 列表依次尝试路径（等价 Nginx try_files $uri $uri/ /fallback.html =404）
///
/// 支持的条目格式：
/// - `$uri`      → 请求路径对应的文件
/// - `$uri/`     → 请求路径对应的目录（查找 index 文件）
/// - `/path`     → 固定路径的文件
/// - `=<code>`   → 返回指定 HTTP 状态码（必须是最后一项）
/// 供 http.rs 调用（root 为 Option）
pub async fn try_files_resolve(
    try_files_list: &[String],
    request_path: &str,
    root: Option<&std::path::PathBuf>,
) -> TryFilesResult {
    let index_files = vec!["index.php".to_string(), "index.html".to_string()];
    let root_path = match root {
        Some(r) => r.as_path(),
        None => return TryFilesResult::NotFound,
    };
    try_files_resolve_inner(root_path, request_path, try_files_list, &index_files).await
}

/// 内部实现（保持原始变体不变）
async fn try_files_resolve_inner(
    root: &Path,
    request_path: &str,
    try_files: &[String],
    index_files: &[String],
) -> TryFilesResult {
    let uri = request_path.split('?').next().unwrap_or(request_path);

    for entry in try_files {
        let entry = entry.trim();

        // =<code>：返回状态码
        if let Some(code_str) = entry.strip_prefix('=') {
            if let Ok(code) = code_str.parse::<u16>() {
                return TryFilesResult::Code(code);
            }
            continue;
        }

        // 展开变量
        let expanded = entry
            .replace("$uri", uri)
            .replace("$request_uri", request_path);

        if expanded.ends_with('/') {
            // 目录模式：查找 index 文件
            let dir_path = match resolve_safe_path(root, expanded.trim_end_matches('/')) {
                Some(p) => p,
                None => continue,
            };
            if let Some(idx) = find_index(&dir_path, index_files).await {
                return TryFilesResult::File(idx);
            }
        } else {
            // 文件模式
            let file_path = match resolve_safe_path(root, &expanded) {
                Some(p) => p,
                None => continue,
            };
            if file_path.is_file() {
                return TryFilesResult::File(file_path);
            }
        }
    }

    TryFilesResult::NotFound
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
