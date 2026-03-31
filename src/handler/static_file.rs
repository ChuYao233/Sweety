//! 静态文件处理器
//!
//! # 压缩策略
//! - **Brotli**：当客户端支持 `br` 时优先使用，压缩率比 gzip 高 20-30%
//! - **gzip**：客户端不支持 br 时的降级选项
//! - **都仅对 ≤ 4MB 小文件做内存压缩，大文件直接流式传输**

use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;

use bytes::Bytes;
use dashmap::DashMap;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::debug;
use sweety_web::{
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
/// 内存文件缓存大小上限：只缓存 ≤ 256KB 的小文件
const FILE_CACHE_MAX_BYTES: u64 = 256 * 1024;
/// 最多缓存条数
const FILE_CACHE_MAX_ENTRIES: usize = 4096;

/// 文件内存缓存条目（预缓存所有响应头 HeaderValue，缓存命中时零分配）
#[derive(Clone)]
struct FileCacheEntry {
    data:          Bytes,
    modified_secs: u64,
    // 预构建的响应头（缓存命中时直接插入，零 from_str 分配）
    hv_content_type:  HeaderValue,
    hv_content_length: HeaderValue,
    hv_etag:          HeaderValue,
    hv_last_modified: HeaderValue,
    hv_cache_control: HeaderValue,
}

/// 全局文件内存缓存（DashMap：无锁并发读写，高并发下不需要 RwLock）
static FILE_CACHE: OnceLock<DashMap<PathBuf, FileCacheEntry>> = OnceLock::new();

fn file_cache() -> &'static DashMap<PathBuf, FileCacheEntry> {
    FILE_CACHE.get_or_init(|| DashMap::with_capacity(FILE_CACHE_MAX_ENTRIES))
}

/// 启动文件变更监听：文件修改/删除时直接淡化 FILE_CACHE 对应条目
/// 返回 watcher 句柄，调用方需保持其生命周期与服务器相同
pub fn start_file_cache_watcher(roots: Vec<PathBuf>) -> Option<RecommendedWatcher> {
    if roots.is_empty() { return None; }

    let watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        // 只处理文件内容变化事件（写入、创建、删除、重命名）
        if !matches!(event.kind,
            EventKind::Modify(_) | EventKind::Create(_)
            | EventKind::Remove(_)
        ) { return; }

        let cache = file_cache();
        for path in &event.paths {
            // 直接移除命中的缓存条目，下次请求将重新从磁盘加载
            if cache.remove(path).is_some() {
                debug!("文件缓存已淡化: {}", path.display());
            }
        }
    });

    match watcher {
        Ok(mut w) => {
            for root in &roots {
                if let Err(e) = w.watch(root, RecursiveMode::Recursive) {
                    tracing::warn!("文件缓存监听启动失败 ({}): {}", root.display(), e);
                }
            }
            tracing::info!("文件缓存 notify 监听已启动，共 {} 个目录", roots.len());
            Some(w)
        }
        Err(e) => {
            tracing::warn!("文件缓存 notify 初始化失败: {}，回退到定期检查模式", e);
            None
        }
    }
}


/// 处理静态文件请求（sweety-web WebContext 版本）
pub async fn handle_sweety(
    ctx: &WebContext<'_, AppState>,
    site: &SiteInfo,
    location: &LocationConfig,
) -> WebResponse {
    // 直接导用 URI 中的路径，避免 to_string() 堆分配
    let path = ctx.req().uri().path();
    let method = ctx.req().method().as_str();
    let req_headers = ctx.req().headers();

    // 确定文件系统根目录（location 级 root 优先于 site 级 root）
    let root: &Path = match location.root.as_deref().or(site.root.as_deref()) {
        Some(r) => r,
        None => {
            return make_error(StatusCode::INTERNAL_SERVER_ERROR, "站点未配置 root 目录");
        }
    };

    // 安全路径解析（防目录穿越）
    // 使用预计算的 canonical_root，跳过每请求 root.canonicalize() 系统调用
    let canonical_root_ref = site.canonical_root.as_deref();
    let file_path = match resolve_safe_path_fast(root, path, canonical_root_ref) {
        Some(p) => p,
        None => return make_error(StatusCode::FORBIDDEN, "Forbidden"),
    };

    // 目录：尝试默认文档（用 tokio::fs::metadata 避免阻塞 worker 线程）
    let meta = tokio::fs::metadata(&file_path).await;
    let was_dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
    let file_path = if was_dir {
        match find_index(&file_path, &site.index).await {
            Some(p) => p,
            None => return make_error(StatusCode::FORBIDDEN, "Directory listing disabled"),
        }
    } else {
        file_path
    };

    // 文件不存在时应用 try_files（等价 Nginx try_files $uri $uri/ /index.html）
    // 目录路径已找到 index 文件，跳过此检查；非目录路径用已有 meta 判断
    let file_path = if !was_dir && !meta.as_ref().map(|m| m.is_file()).unwrap_or(false) {
        if !location.try_files.is_empty() {
            match try_files_resolve_inner(root, path, &location.try_files, &site.index).await {
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

    // ── 小文件内存缓存（单层，热路径：完全跳过 metadata + open 系统调用）─────────
    // 仅对普通 GET（非 Range、非 HEAD）的小文件启用；Range / HEAD / 大文件走下方磁盘路径
    let is_range_req = req_headers.get(sweety_web::http::header::RANGE).is_some();
    let is_head      = method.eq_ignore_ascii_case("HEAD");

    if !is_head && !is_range_req {
        let cache = file_cache();
        if let Some(entry) = cache.get(&file_path) {
            let modified_secs = entry.modified_secs;
            let hv_ct  = entry.hv_content_type.clone();
            let hv_cl  = entry.hv_content_length.clone();
            let hv_et  = entry.hv_etag.clone();
            let hv_lm  = entry.hv_last_modified.clone();
            let hv_cc  = entry.hv_cache_control.clone();
            // etag 直接从预构建的 HeaderValue 拿 str，避免单独 clone String
            let etag_str = hv_et.to_str().unwrap_or("");
            let data   = entry.data.clone();
            drop(entry); // 尽早释放 DashMap 读槽

            // 304 协商缓存
            let if_none_match     = req_headers.get(IF_NONE_MATCH).and_then(|v| v.to_str().ok());
            let if_modified_since = req_headers.get(IF_MODIFIED_SINCE)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| httpdate::parse_http_date(s).ok())
                .map(to_unix_secs);
            if crate::middleware::cache::is_cache_valid(if_none_match, if_modified_since, etag_str, modified_secs) {
                let mut resp = WebResponse::new(ResponseBody::none());
                *resp.status_mut() = StatusCode::NOT_MODIFIED;
                return resp;
            }
            // 直接插入预构建的头，零 from_str 分配
            let mut resp = WebResponse::new(ResponseBody::from(data));
            let h = resp.headers_mut();
            h.insert(CONTENT_TYPE,   hv_ct);
            h.insert(CONTENT_LENGTH, hv_cl);
            h.insert(ETAG,           hv_et);
            h.insert(LAST_MODIFIED,  hv_lm);
            h.insert(CACHE_CONTROL,  hv_cc);
            return resp;
        }
        // 缓存未命中：先读 metadata 确认大小，≤ FILE_CACHE_MAX_BYTES 则读入内存并缓存
        let meta = match tokio::fs::metadata(&file_path).await {
            Ok(m) => m,
            Err(e) => {
                tracing::error!("读取文件元数据失败 {}: {}", file_path.display(), e);
                return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
            }
        };
        let file_size     = meta.len();
        let modified_secs = to_unix_secs(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH));
        let etag_val      = generate_etag(file_size, modified_secs);

        let if_none_match     = req_headers.get(IF_NONE_MATCH).and_then(|v| v.to_str().ok());
        let if_modified_since = req_headers.get(IF_MODIFIED_SINCE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| httpdate::parse_http_date(s).ok())
            .map(to_unix_secs);
        if crate::middleware::cache::is_cache_valid(if_none_match, if_modified_since, &etag_val, modified_secs) {
            let mut resp = WebResponse::new(ResponseBody::none());
            *resp.status_mut() = StatusCode::NOT_MODIFIED;
            return resp;
        }

        if file_size <= FILE_CACHE_MAX_BYTES {
            let data = match tokio::fs::read(&file_path).await {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("读取文件失败 {}: {}", file_path.display(), e);
                    return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
                }
            };
            // 超出条数上限时淘汰最旧 1/4
            if cache.len() >= FILE_CACHE_MAX_ENTRIES {
                let to_remove: Vec<_> = cache.iter()
                    .take(FILE_CACHE_MAX_ENTRIES / 4)
                    .map(|e| e.key().clone())
                    .collect();
                for k in to_remove { cache.remove(&k); }
            }
            let bytes = Bytes::from(data);
            // 写入缓存时预构建所有 HeaderValue，后续命中直接用零分配
            let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let mime = mime_type_for(ext);
            let modified_time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(modified_secs);
            let http_date = httpdate::fmt_http_date(modified_time);
            let cc = location.cache_control.as_deref().unwrap_or_else(|| default_cache_control(ext));
            let mut cl_buf = itoa::Buffer::new();
            let entry = FileCacheEntry {
                data:               bytes.clone(),
                modified_secs,
                hv_content_type:    HeaderValue::from_str(mime).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
                hv_content_length:  HeaderValue::from_str(cl_buf.format(file_size)).unwrap_or_else(|_| HeaderValue::from_static("0")),
                hv_etag:            HeaderValue::from_str(&etag_val).unwrap_or_else(|_| HeaderValue::from_static("\"\"") ),
                hv_last_modified:   HeaderValue::from_str(&http_date).unwrap_or_else(|_| HeaderValue::from_static("Thu, 01 Jan 1970 00:00:00 GMT")),
                hv_cache_control:   HeaderValue::from_str(cc).unwrap_or_else(|_| HeaderValue::from_static("public, max-age=3600")),
            };
            // 先 clone 各头值，再 move entry 进缓存（避免整个 entry 二次 clone）
            let hv_ct = entry.hv_content_type.clone();
            let hv_cl = entry.hv_content_length.clone();
            let hv_et = entry.hv_etag.clone();
            let hv_lm = entry.hv_last_modified.clone();
            let hv_cc = entry.hv_cache_control.clone();
            cache.insert(file_path.clone(), entry);
            let mut resp = WebResponse::new(ResponseBody::from(bytes));
            let h = resp.headers_mut();
            h.insert(CONTENT_TYPE,   hv_ct);
            h.insert(CONTENT_LENGTH, hv_cl);
            h.insert(ETAG,           hv_et);
            h.insert(LAST_MODIFIED,  hv_lm);
            h.insert(CACHE_CONTROL,  hv_cc);
            return resp;
        }

        // 大文件（> FILE_CACHE_MAX_BYTES）：直接流式传输（不缓存内容）
        debug!("提供静态文件: {} size={}", file_path.display(), file_size);
        let file = match tokio::fs::File::open(&file_path).await {
            Ok(f) => f,
            Err(e) => {
                tracing::error!("打开文件失败 {}: {}", file_path.display(), e);
                return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
            }
        };
        // 大文件不压缩，直接流式传输
        return stream_file_response(file, &file_path, file_size, &etag_val, modified_secs, location);
    }

    // ── HEAD / Range 路径：必须读 metadata ────────────────────────────────────
    let meta = match tokio::fs::metadata(&file_path).await {
        Ok(m) => m,
        Err(e) => {
            tracing::error!("读取文件元数据失败 {}: {}", file_path.display(), e);
            return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
        }
    };

    let file_size     = meta.len();
    let modified_secs = to_unix_secs(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH));
    let etag_val      = generate_etag(file_size, modified_secs);

    // 304 缓存验证
    let if_none_match = req_headers.get(IF_NONE_MATCH).and_then(|v| v.to_str().ok());
    let if_modified_since = req_headers
        .get(IF_MODIFIED_SINCE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| httpdate::parse_http_date(s).ok())
        .map(to_unix_secs);

    if crate::middleware::cache::is_cache_valid(if_none_match, if_modified_since, &etag_val, modified_secs) {
        let mut resp = WebResponse::new(ResponseBody::none());
        *resp.status_mut() = StatusCode::NOT_MODIFIED;
        return resp;
    }

    // HEAD 请求只返回头信息
    if is_head {
        let mut resp = WebResponse::new(ResponseBody::none());
        set_file_headers(resp.headers_mut(), &file_path, file_size, &etag_val, modified_secs, location);
        return resp;
    }

    // 解析 Range 头（bytes=start-end，只支持单区间）
    let range = req_headers
        .get(sweety_web::http::header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| parse_range(s, file_size));

    debug!("提供静态文件: {} size={} range={:?}", file_path.display(), file_size, range);

    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

    // 打开文件（大文件 / Range / HEAD）
    let mut file = match tokio::fs::File::open(&file_path).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("打开文件失败 {}: {}", file_path.display(), e);
            return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
        }
    };

    if let Some((range_start, range_end)) = range {
        // ── Range 请求：seek + Take 限制范围，流式传输 ──────────────────
        let range_len = range_end - range_start + 1;
        if file.seek(SeekFrom::Start(range_start)).await.is_err() {
            return make_error(StatusCode::RANGE_NOT_SATISFIABLE, "");
        }
        // Range 请求也用背压 stream，防止 H2 flow control buffer 溢出
        let limited = file.take(range_len);
        let stream = crate::handler::sendfile::file_stream_backpressure(limited, range_len);
        let body = ResponseBody::box_stream(stream);
        let mut resp = WebResponse::new(body);
        *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
        set_file_headers(resp.headers_mut(), &file_path, range_len, &etag_val, modified_secs, location);
        // Content-Range 用 push_str 拼接，避免 format! 堆分配
        let mut cr = String::with_capacity(32);
        cr.push_str("bytes ");
        cr.push_str(itoa::Buffer::new().format(range_start));
        cr.push('-');
        cr.push_str(itoa::Buffer::new().format(range_end));
        cr.push('/');
        cr.push_str(itoa::Buffer::new().format(file_size));
        if let Ok(v) = HeaderValue::from_str(&cr) {
            resp.headers_mut().insert(CONTENT_RANGE, v);
        }
        resp
    } else {
        // ── 普通请求 ────────────────────────────────────────────────────────────────
        let global = &ctx.state().cfg.global;
        let gzip_enabled = site.gzip.unwrap_or(global.gzip);
        let gzip_level = site.gzip_comp_level.unwrap_or(global.gzip_comp_level);
        let min_bytes = (global.gzip_min_length as u64) * 1024;
        let accept_enc = req_headers
            .get(ACCEPT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let accept_br = accept_enc.contains("br");
        let accept_gz = accept_enc.contains("gzip");
        let already_compressed = matches!(ext,
            "gz" | "br" | "zst" | "zip" | "png" | "jpg" | "jpeg"
            | "gif" | "webp" | "avif" | "mp4" | "webm" | "woff" | "woff2"
            | "bin" | "dat" | "raw" | "iso" | "exe" | "dll" | "so"
        );
        let can_compress = gzip_enabled && !already_compressed
            && file_size >= min_bytes && file_size <= GZIP_MAX_INLINE;

        // Brotli 优先（压缩率比 gzip 高 20-30%）
        if can_compress && accept_br {
            let mut raw = Vec::with_capacity(file_size as usize);
            if let Err(e) = file.read_to_end(&mut raw).await {
                tracing::error!("读取文件失败 {}: {}", file_path.display(), e);
                return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
            }
            match brotli_compress(&raw).await {
                Ok(compressed) => {
                    let clen = compressed.len() as u64;
                    let mut resp = WebResponse::new(ResponseBody::from(compressed));
                    set_file_headers(resp.headers_mut(), &file_path, clen, &etag_val, modified_secs, location);
                    resp.headers_mut().insert(CONTENT_ENCODING, HeaderValue::from_static("br"));
                    return resp;
                }
                Err(_) => {
                    // Brotli 失败降级到 gzip 或直接流式
                    match tokio::fs::File::open(&file_path).await {
                        Ok(f2) => return stream_file_response(f2, &file_path, file_size, &etag_val, modified_secs, location),
                        Err(_) => return make_error(StatusCode::INTERNAL_SERVER_ERROR, ""),
                    }
                }
            }
        }

        // gzip：客户端不支持 br 时的降级选项
        if can_compress && accept_gz {
            let mut raw = Vec::with_capacity(file_size as usize);
            if let Err(e) = file.read_to_end(&mut raw).await {
                tracing::error!("读取文件失败 {}: {}", file_path.display(), e);
                return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
            }
            let compress_result = gzip_compress(&raw, gzip_level);
            drop(raw);
            match compress_result {
                Ok(compressed) => {
                    let clen = compressed.len() as u64;
                    let mut resp = WebResponse::new(ResponseBody::from(compressed));
                    set_file_headers(resp.headers_mut(), &file_path, clen, &etag_val, modified_secs, location);
                    resp.headers_mut().insert(CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                    return resp;
                }
                Err(_) => {
                    match tokio::fs::File::open(&file_path).await {
                        Ok(f2) => return stream_file_response(f2, &file_path, file_size, &etag_val, modified_secs, location),
                        Err(_) => return make_error(StatusCode::INTERNAL_SERVER_ERROR, ""),
                    }
                }
            }
        }

        // 大文件 / 不压缩 / 已压缩格式：ReaderStream 流式传输
        stream_file_response(file, &file_path, file_size, &etag_val, modified_secs, location)
    }
}

/// 把打开的文件包装为流式 ResponseBody
///
/// - 使用带背压的 bounded channel stream（容量 = 2）
/// - 生产者每发一个 256KB chunk 就 await，等消费者（H2 framer）拉取后才继续读
/// - 内存恒定 ≤ 2 × 256KB = 512KB，无论文件多大，H2 flow control buffer 不溢出
fn stream_file_response(
    file: tokio::fs::File,
    file_path: &Path,
    file_size: u64,
    etag_val: &str,
    modified_secs: u64,
    location: &LocationConfig,
) -> WebResponse {
    let stream = crate::handler::sendfile::file_stream_backpressure(file, file_size);
    let body = ResponseBody::box_stream(stream);
    let mut resp = WebResponse::new(body);
    set_file_headers(resp.headers_mut(), file_path, file_size, etag_val, modified_secs, location);
    resp
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

/// Brotli 压缩（async-compression，spawn_blocking 避免阻塞 tokio 线程）
async fn brotli_compress(data: &[u8]) -> std::io::Result<bytes::Bytes> {
    use async_compression::tokio::bufread::BrotliEncoder;
    use tokio::io::AsyncReadExt;

    let cursor = std::io::Cursor::new(data.to_vec());
    let reader = tokio::io::BufReader::new(cursor);
    let mut encoder = BrotliEncoder::new(reader);
    let mut out = Vec::with_capacity(data.len() / 3);
    encoder.read_to_end(&mut out).await?;
    Ok(bytes::Bytes::from(out))
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

    // Content-Length（用栈缓冲区避免 to_string() 堆分配）
    let mut cl_buf = itoa::Buffer::new();
    if let Ok(v) = HeaderValue::from_str(cl_buf.format(size)) {
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
///
/// `canonical_root`：启动时预计算的 `root.canonicalize()` 结果，
/// 传入后跳过每请求 `root.canonicalize()` 系统调用（约省 1 次 stat syscall）。
pub fn resolve_safe_path(root: &Path, request_path: &str) -> Option<PathBuf> {
    resolve_safe_path_with_canon(root, request_path, None)
}

/// 带预计算 canonical root 的版本（供 handle_sweety 调用以消除重复系统调用）
pub fn resolve_safe_path_fast(
    root: &Path,
    request_path: &str,
    canonical_root: Option<&Path>,
) -> Option<PathBuf> {
    resolve_safe_path_with_canon(root, request_path, canonical_root)
}

fn resolve_safe_path_with_canon(
    root: &Path,
    request_path: &str,
    canonical_root: Option<&Path>,
) -> Option<PathBuf> {
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
    let cr_opt: Option<PathBuf>;
    let cr = match canonical_root {
        Some(p) => Some(p),     // 使用预计算结果，跳过 syscall
        None => {
            cr_opt = root.canonicalize().ok();
            cr_opt.as_deref()
        }
    };
    match (full.canonicalize().ok(), cr) {
        (Some(cf), Some(cr)) => {
            if cf.starts_with(cr) { Some(cf) } else { None }
        }
        _ => Some(full),
    }
}

/// 构造简单错误响应（使用预构建 Bytes 缓存，零堆分配）
#[inline(always)]
fn make_error(status: StatusCode, _msg: &str) -> WebResponse {
    let body = crate::handler::error_page::get_error_bytes(status.as_u16());
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
