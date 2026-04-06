//! 静态文件处理器
//!
//! # 压缩策略（优先级 br > zstd > gzip）
//! - **Brotli**：客户端支持 `br` 时优先，压缩率最高
//! - **zstd**：客户端支持 `zstd` 时次选，解压速度最快
//! - **gzip**：通用降级选项，兼容性最好
//! - 三者均仅对 ≤ 1MB 小文件做内存预压缩，大文件直接流式传输

use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tracing::debug;
use bytes::Bytes;
use sweety_web::{
    body::ResponseBody,
    http::{
        StatusCode, WebResponse,
        header::{
            ACCEPT_ENCODING, ACCEPT_RANGES, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_RANGE,
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

mod cache;
mod compress;
mod path;
mod range;

pub use cache::start_file_cache_watcher;
pub use path::{TryFilesResult, try_files_resolve, resolve_safe_path, resolve_safe_path_fast};

use cache::{
    FileCacheEntry, cache_get_fast, cache_insert, make_cache_key,
    fd_cache_get_or_open_arc, make_cache_key_from_path,
    GZIP_MAX_INLINE, FILE_CACHE_MAX_BYTES,
};
use compress::{stream_file_response_pread, gzip_compress, brotli_compress, zstd_compress};
use range::parse_range;

/// 根据 Accept-Encoding 头和缓存条目选出最优编码
/// 优先级：br > zstd > gzip > 原始
#[inline]
fn pick_encoding<'a>(
    accept_enc: &str,
    entry: &'a FileCacheEntry,
) -> (&'a Bytes, Option<&'static str>) {
    if accept_enc.contains("br") {
        if let Some(b) = &entry.br { return (b, Some("br")); }
    }
    if accept_enc.contains("zstd") {
        if let Some(b) = &entry.zst { return (b, Some("zstd")); }
    }
    if accept_enc.contains("gzip") {
        if let Some(b) = &entry.gz { return (b, Some("gzip")); }
    }
    (&entry.data, None)
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

    let is_range_req = req_headers.get(sweety_web::http::header::RANGE).is_some();
    let is_head      = method.eq_ignore_ascii_case("HEAD");

    // ── 小文件内存缓存热路径 ────────────────────────────────────────────────────
    // 热路径：先用无 canonicalize 的快速路径构建 key 查缓存
    // 缓存 key 在写入时已经是 canonical path，查缓存只需 root.join + cache.get
    // 命中时完全跳过 canonicalize/stat/open 系统调用
    // Range 请求也走此热路径：小文件数据在内存中，直接 slice，零 syscall
    if !is_head {
        // 快速 `..` 检查（不做 syscall）
        let path_only = path.split('?').next().unwrap_or(path);
        let has_traversal = path_only.split('/').any(|s| s == "..");
        if has_traversal {
            return make_error(StatusCode::FORBIDDEN, "Forbidden");
        }
        let relative = path_only.trim_start_matches('/');
        if let Some(entry) = cache_get_fast(root, relative) {
            let modified_secs = entry.modified_secs;
            let etag_str = entry.hv_etag.to_str().unwrap_or("");

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

            // ── Range 请求：直接从内存缓存 slice，零 open/seek syscall ─────────
            if is_range_req {
                let file_size = entry.data.len() as u64;
                let range_hdr = req_headers
                    .get(sweety_web::http::header::RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| parse_range(s, file_size));
                if let Some((range_start, range_end)) = range_hdr {
                    let range_len = range_end - range_start + 1;
                    let slice = entry.data.slice(range_start as usize..=range_end as usize);
                    let mut resp = WebResponse::new(ResponseBody::from(slice));
                    *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
                    let h = resp.headers_mut();
                    // 直接用缓存里预构建的头，零分配
                    h.insert(CONTENT_TYPE, entry.hv_content_type.clone());
                    h.insert(ETAG, entry.hv_etag.clone());
                    h.insert(LAST_MODIFIED, entry.hv_last_modified.clone());
                    h.insert(CACHE_CONTROL, entry.hv_cache_control.clone());
                    h.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
                    if let Ok(v) = HeaderValue::from_str(itoa::Buffer::new().format(range_len)) {
                        h.insert(CONTENT_LENGTH, v);
                    }
                    let mut cr = String::with_capacity(32);
                    cr.push_str("bytes "); cr.push_str(itoa::Buffer::new().format(range_start));
                    cr.push('-'); cr.push_str(itoa::Buffer::new().format(range_end));
                    cr.push('/'); cr.push_str(itoa::Buffer::new().format(file_size));
                    if let Ok(v) = HeaderValue::from_str(&cr) { h.insert(CONTENT_RANGE, v); }
                    return resp;
                }
                // Range 解析失败（超出范围等），回落到下方常规路径
            }

            // 选择最优编码（br > zstd > gzip > 原始），复用缓存中预压缩结果
            let accept_enc = req_headers.get(ACCEPT_ENCODING).and_then(|v| v.to_str().ok()).unwrap_or("");
            let (body_bytes, enc) = pick_encoding(accept_enc, &entry);
            let body_bytes = body_bytes.clone();

            // 直接插入预构建的头，零 from_str 分配
            let mut resp = WebResponse::new(ResponseBody::from(body_bytes.clone()));
            let h = resp.headers_mut();
            h.insert(CONTENT_TYPE,   entry.hv_content_type.clone());
            h.insert(ACCEPT_RANGES,  HeaderValue::from_static("bytes"));
            if enc.is_some() {
                if let Ok(v) = HeaderValue::from_str(itoa::Buffer::new().format(body_bytes.len())) {
                    h.insert(CONTENT_LENGTH, v);
                }
            } else {
                h.insert(CONTENT_LENGTH, entry.hv_content_length.clone());
            }
            h.insert(ETAG,           entry.hv_etag.clone());
            h.insert(LAST_MODIFIED,  entry.hv_last_modified.clone());
            h.insert(CACHE_CONTROL,  entry.hv_cache_control.clone());
            if let Some(e) = enc {
                if let Ok(v) = HeaderValue::from_str(e) { h.insert(CONTENT_ENCODING, v); }
            }
            return resp;
        }
        // 缓存未命中：做完整安全检查，然后读文件并缓存
        let canonical_root_ref = site.canonical_root.as_deref();
        let file_path = match resolve_safe_path_fast(root, path, canonical_root_ref) {
            Some(p) => p,
            None => return make_error(StatusCode::FORBIDDEN, "Forbidden"),
        };
        let meta = match tokio::fs::metadata(&file_path).await {
            Ok(m) => m,
            Err(_) => return make_error(StatusCode::NOT_FOUND, ""),
        };

        // 目录：按 site.index 列表查找默认文档
        let file_path = if meta.is_dir() {
            let mut found: Option<PathBuf> = None;
            for name in &site.index {
                let candidate = file_path.join(name);
                if candidate.is_file() {
                    found = Some(candidate);
                    break;
                }
            }
            match found {
                Some(p) => p,
                None => return make_error(StatusCode::FORBIDDEN, "Directory listing not allowed"),
            }
        } else {
            file_path
        };
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
            let bytes = Bytes::from(data);
            let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let mime = mime_type_for(ext);

            // 读取有效压缩配置：站点覆盖全局；旧字段（gzip/gzip_comp_level）作为 fallback
            let global = &ctx.state().cfg.load().global;
            let eff = site.compress.resolve(&global.compress);
            // 旧字段 fallback：若 compress.gzip 未显式开启，检查旧字段
            let gz_enabled  = eff.gzip  || site.gzip.unwrap_or(global.gzip);
            let gz_level    = if site.compress.gzip_level.is_some() { eff.gzip_level }
                              else { site.gzip_comp_level.unwrap_or(eff.gzip_level) };
            let br_enabled  = eff.brotli;
            let zst_enabled = eff.zstd;
            let min_bytes   = (eff.min_length as u64) * 1024;

            let already_compressed = matches!(ext,
                "gz" | "br" | "zst" | "zip" | "png" | "jpg" | "jpeg"
                | "gif" | "webp" | "avif" | "mp4" | "webm" | "woff" | "woff2"
                | "bin" | "dat" | "raw" | "iso" | "exe" | "dll" | "so"
            );
            let compressible_mime = mime.starts_with("text/")
                || mime == "application/json"
                || mime == "application/javascript"
                || mime == "application/x-javascript"
                || mime == "application/xml"
                || mime == "application/xhtml+xml"
                || mime == "application/rss+xml"
                || mime == "application/atom+xml"
                || mime == "image/svg+xml";
            let can_compress = !already_compressed && compressible_mime
                && file_size >= min_bytes && file_size <= GZIP_MAX_INLINE;

            // 并发预压缩：仅启用的算法才运行，未启用的直接跳过
            let gz_bytes = if gz_enabled && can_compress {
                let raw = bytes.clone();
                tokio::task::spawn_blocking(move || gzip_compress(&raw, gz_level))
                    .await.ok().and_then(|r| r.ok())
            } else { None };
            let br_bytes = if br_enabled && can_compress {
                brotli_compress(&bytes, eff.brotli_level).await.ok()
            } else { None };
            let zst_bytes = if zst_enabled && can_compress {
                zstd_compress(&bytes, eff.zstd_level).await.ok()
            } else { None };

            let modified_time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(modified_secs);
            let http_date = httpdate::fmt_http_date(modified_time);
            let cc = location.cache_control.as_deref().unwrap_or_else(|| default_cache_control(ext));
            let mut cl_buf = itoa::Buffer::new();
            let entry = FileCacheEntry {
                data:               bytes.clone(),
                gz:                 gz_bytes,
                br:                 br_bytes,
                zst:                zst_bytes,
                modified_secs,
                hv_content_type:    HeaderValue::from_str(mime).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
                hv_content_length:  HeaderValue::from_str(cl_buf.format(file_size)).unwrap_or_else(|_| HeaderValue::from_static("0")),
                hv_etag:            HeaderValue::from_str(&etag_val).unwrap_or_else(|_| HeaderValue::from_static("\"\"") ),
                hv_last_modified:   HeaderValue::from_str(&http_date).unwrap_or_else(|_| HeaderValue::from_static("Thu, 01 Jan 1970 00:00:00 GMT")),
                hv_cache_control:   HeaderValue::from_str(cc).unwrap_or_else(|_| HeaderValue::from_static("public, max-age=3600")),
            };
            // 选择最优编码写缓存并直接返回
            let accept_enc2 = req_headers.get(ACCEPT_ENCODING).and_then(|v| v.to_str().ok()).unwrap_or("");
            let (resp_bytes_ref, enc_name) = pick_encoding(accept_enc2, &entry);
            let resp_bytes = resp_bytes_ref.clone();
            let enc_hv = enc_name.map(|n| HeaderValue::from_static(n));
            let hv_ct = entry.hv_content_type.clone();
            let hv_cl = entry.hv_content_length.clone();
            let hv_et = entry.hv_etag.clone();
            let hv_lm = entry.hv_last_modified.clone();
            let hv_cc = entry.hv_cache_control.clone();
            // 同时插入 canonical key 和 fast_key，保证两条查询路径都能命中
            let canonical_key = make_cache_key_from_path(&file_path);
            cache_insert(canonical_key.clone(), entry.clone());
            let fast_key_str = make_cache_key(root, relative);
            if fast_key_str != canonical_key { cache_insert(fast_key_str, entry); }
            let mut resp = WebResponse::new(ResponseBody::from(resp_bytes.clone()));
            let h = resp.headers_mut();
            h.insert(CONTENT_TYPE,   hv_ct);
            h.insert(ACCEPT_RANGES,  HeaderValue::from_static("bytes"));
            if enc_hv.is_some() {
                if let Ok(v) = HeaderValue::from_str(itoa::Buffer::new().format(resp_bytes.len())) {
                    h.insert(CONTENT_LENGTH, v);
                }
            } else {
                h.insert(CONTENT_LENGTH, hv_cl);
            }
            h.insert(ETAG,           hv_et);
            h.insert(LAST_MODIFIED,  hv_lm);
            h.insert(CACHE_CONTROL,  hv_cc);
            if let Some(enc) = enc_hv { h.insert(CONTENT_ENCODING, enc); }
            return resp;
        }

        // 大文件（> FILE_CACHE_MAX_BYTES）：先查 fd 缓存，命中则共享 fd 直接用；否则 open 后写入缓存
        debug!("提供静态文件(stream): {} size={} range={}", file_path.display(), file_size, is_range_req);
        let arc_fd = match fd_cache_get_or_open_arc(&file_path, file_size, modified_secs).await {
            Some(f) => f,
            None => {
                tracing::error!("打开文件失败: {}", file_path.display());
                return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
            }
        };

        // Range 请求：只传 Range 片段
        if is_range_req {
            let range = req_headers
                .get(sweety_web::http::header::RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| parse_range(s, file_size));
            if let Some((range_start, range_end)) = range {
                let range_len = range_end - range_start + 1;
                // Range ≤ 4MB：单次 pread 读满到堆内存，零 stream 调度开销
                // 典型场景：视频分块请求（浏览器默认 128KB-1MB/块）
                const PREAD_INLINE_MAX: u64 = 4 * 1024 * 1024;
                let body = if range_len <= PREAD_INLINE_MAX {
                    match crate::handler::sendfile::async_read_range(&arc_fd, range_start, range_len as usize).await {
                        Ok(buf) => ResponseBody::from(buf),
                        Err(_) => return make_error(StatusCode::INTERNAL_SERVER_ERROR, ""),
                    }
                } else {
                    let stream = crate::handler::sendfile::pread_stream(arc_fd, range_start, range_len);
                    ResponseBody::box_stream(stream)
                };
                let mut resp = WebResponse::new(body);
                *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
                set_file_headers(resp.headers_mut(), &file_path, range_len, &etag_val, modified_secs, location);
                let mut cr = String::with_capacity(32);
                cr.push_str("bytes "); cr.push_str(itoa::Buffer::new().format(range_start));
                cr.push('-'); cr.push_str(itoa::Buffer::new().format(range_end));
                cr.push('/'); cr.push_str(itoa::Buffer::new().format(file_size));
                if let Ok(v) = HeaderValue::from_str(&cr) { resp.headers_mut().insert(CONTENT_RANGE, v); }
                return resp;
            }
            // Range 解析失败（超出范围），回落到全量传输
        }

        return stream_file_response_pread(arc_fd, &file_path, file_size, 0, file_size, &etag_val, modified_secs, location);
    }

    // ── HEAD / Range 路径：必须读 metadata ────────────────────────────────────
    let canonical_root_ref = site.canonical_root.as_deref();
    let file_path = match resolve_safe_path_fast(root, path, canonical_root_ref) {
        Some(p) => p,
        None => return make_error(StatusCode::FORBIDDEN, "Forbidden"),
    };
    let meta = match tokio::fs::metadata(&file_path).await {
        Ok(m) => m,
        Err(_) => return make_error(StatusCode::NOT_FOUND, ""),
    };

    // 目录：按 site.index 列表查找默认文档
    let file_path = if meta.is_dir() {
        let mut found: Option<PathBuf> = None;
        for name in &site.index {
            let candidate = file_path.join(name);
            if candidate.is_file() {
                found = Some(candidate);
                break;
            }
        }
        match found {
            Some(p) => p,
            None => return make_error(StatusCode::FORBIDDEN, "Directory listing not allowed"),
        }
    } else {
        file_path
    };
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

    let _ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

    if let Some((range_start, range_end)) = range {
        // ── Range 请求 ───────────────────────────────────────────────────────────────────
        let range_len = range_end - range_start + 1;
        // 查 fd 缓存：命中则共享 fd，未命中则同步 open（随即写入缓存）
        let arc_fd = match fd_cache_get_or_open_arc(&file_path, file_size, modified_secs).await {
            Some(f) => f,
            None => {
                tracing::error!("打开文件失败: {}", file_path.display());
                return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
            }
        };

        // Range ≤ 4MB：单次 pread 读满到堆内存，零 stream 调度开销
        const PREAD_INLINE_MAX: u64 = 4 * 1024 * 1024;
        let body = if range_len <= PREAD_INLINE_MAX {
            match crate::handler::sendfile::async_read_range(&arc_fd, range_start, range_len as usize).await {
                Ok(buf) => ResponseBody::from(buf),
                Err(_) => return make_error(StatusCode::INTERNAL_SERVER_ERROR, ""),
            }
        } else {
            let stream = crate::handler::sendfile::pread_stream(arc_fd, range_start, range_len);
            ResponseBody::box_stream(stream)
        };

        let mut resp = WebResponse::new(body);
        *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
        set_file_headers(resp.headers_mut(), &file_path, range_len, &etag_val, modified_secs, location);
        let mut cr = String::with_capacity(32);
        cr.push_str("bytes "); cr.push_str(itoa::Buffer::new().format(range_start));
        cr.push('-'); cr.push_str(itoa::Buffer::new().format(range_end));
        cr.push('/'); cr.push_str(itoa::Buffer::new().format(file_size));
        if let Ok(v) = HeaderValue::from_str(&cr) { resp.headers_mut().insert(CONTENT_RANGE, v); }
        resp
    } else {
        // ── 普通请求（大文件，> FILE_CACHE_MAX_BYTES）────────────────────────────────────────
        let arc_fd = match fd_cache_get_or_open_arc(&file_path, file_size, modified_secs).await {
            Some(f) => f,
            None => {
                tracing::error!("打开文件失败: {}", file_path.display());
                return make_error(StatusCode::INTERNAL_SERVER_ERROR, "");
            }
        };
        stream_file_response_pread(arc_fd, &file_path, file_size, 0, file_size, &etag_val, modified_secs, location)
    }
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

    // RFC 7233: 声明支持 Range 请求（与 Nginx 行为一致）
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
}

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
