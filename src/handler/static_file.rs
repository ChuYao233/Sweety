//! 静态文件处理器
//!
//! # 压缩策略
//! - **Brotli**：当客户端支持 `br` 时优先使用，压缩率比 gzip 高 20-30%
//! - **gzip**：客户端不支持 br 时的降级选项
//! - **都仅对 ≤ 4MB 小文件做内存压缩，大文件直接流式传输**

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::SystemTime;
use bytes::Bytes;
use dashmap::DashMap;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::io::AsyncReadExt;
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

/// gzip/brotli 内存压缩文件大小上限（512 KB）
/// 超过此值直接流式传输，内存压缩成本过高且意义不大
/// 对齐 Nginx gzip_min_length + 完全内存压缩的默认范围
const GZIP_MAX_INLINE: u64 = 512 * 1024;
/// 内存文件缓存大小上限：只缓存 ≤ 32KB 的小文件（对标 Nginx open_file_cache 的 fd+stat 缓存策略）
/// 超过此阈值的文件改用 mmap 零拷贝流式传输，不占用堆内存
const FILE_CACHE_MAX_BYTES: u64 = 32 * 1024;
/// 最多缓存条数（2048 × 32KB ≈ 64MB 最坏情况）
const FILE_CACHE_MAX_ENTRIES: usize = 2048;

/// 文件内存缓存条目（预缓存所有响应头 HeaderValue，缓存命中时零分配）
#[derive(Clone)]
struct FileCacheEntry {
    /// 原始文件内存（未压缩）
    data:          Bytes,
    /// gzip 预压缩结果（None 表示未压缩或超过阈値）
    gz:            Option<Bytes>,
    /// brotli 预压缩结果
    br:            Option<Bytes>,
    modified_secs: u64,
    // 预构建的响应头（缓存命中时直接插入，零 from_str 分配）
    hv_content_type:  HeaderValue,
    hv_content_length: HeaderValue,  // 原始大小
    hv_etag:          HeaderValue,
    hv_last_modified: HeaderValue,
    hv_cache_control: HeaderValue,
}

/// 全局 L2 文件缓存（DashMap：无锁分片读写，多线程共享）
static FILE_CACHE: OnceLock<DashMap<PathBuf, FileCacheEntry>> = OnceLock::new();

fn file_cache() -> &'static DashMap<PathBuf, FileCacheEntry> {
    FILE_CACHE.get_or_init(|| DashMap::with_capacity(FILE_CACHE_MAX_ENTRIES))
}

/// 大文件 fd 缓存条目：缓存 fd + stat，避免 Range/普通请求重复 open syscall
/// 对标 Nginx open_file_cache 机制
#[derive(Clone)]
struct FdCacheEntry {
    file_size:     u64,
    modified_secs: u64,
    /// Arc<std::fs::File> 允许多请求共享同一 fd，每次 dup2/pread 时无需持锁
    /// pread(2) 是无状态的，不需要 seek，天然支持多请求并发
    fd:            std::sync::Arc<std::fs::File>,
}

/// fd 缓存最多条目数（每条目一个 OS fd，不宜过多）
const FD_CACHE_MAX_ENTRIES: usize = 512;

static FD_CACHE: OnceLock<DashMap<PathBuf, FdCacheEntry>> = OnceLock::new();

fn fd_cache() -> &'static DashMap<PathBuf, FdCacheEntry> {
    FD_CACHE.get_or_init(|| DashMap::with_capacity(FD_CACHE_MAX_ENTRIES))
}

/// 从 fd 缓存获取：直接信任缓存（文件变化由 notify watcher 驱逐），消除每次 stat
/// 返回 Arc<std::fs::File> 共享 fd，调用方用 pread(无 seek 竞争)或 dup 后 seek
#[inline]
fn fd_cache_get(path: &PathBuf) -> Option<FdCacheEntry> {
    fd_cache().get(path).map(|e| e.clone())
}

/// 从 fd 缓存获取或 open 文件（异步版，用于首次 open）
/// 命中时直接返回 Arc<std::fs::File>，未命中时 open 并插入缓存
#[inline]
async fn fd_cache_get_or_open_arc(path: &PathBuf, file_size: u64, modified_secs: u64)
    -> Option<std::sync::Arc<std::fs::File>>
{
    if let Some(entry) = fd_cache_get(path) {
        return Some(entry.fd);
    }
    // 未命中：open 并插入缓存
    match std::fs::File::open(path) {
        Ok(f) => {
            let arc = std::sync::Arc::new(f);
            fd_cache_insert_arc(path.clone(), arc.clone(), file_size, modified_secs);
            Some(arc)
        }
        Err(_) => None,
    }
}

/// 插入 fd 缓存（Arc<std::fs::File> 版），LRU 满时淘汰前 1/4
#[inline]
fn fd_cache_insert_arc(path: PathBuf, fd: std::sync::Arc<std::fs::File>, file_size: u64, modified_secs: u64) {
    let cache = fd_cache();
    if cache.len() >= FD_CACHE_MAX_ENTRIES {
        let to_remove: Vec<_> = cache.iter()
            .take(FD_CACHE_MAX_ENTRIES / 4)
            .map(|e| e.key().clone())
            .collect();
        for k in to_remove { cache.remove(&k); }
    }
    cache.insert(path, FdCacheEntry {
        file_size,
        modified_secs,
        fd,
    });
}

#[inline]
fn cache_get(key: &PathBuf) -> Option<FileCacheEntry> {
    file_cache().get(key).map(|e| e.clone())
}

#[inline]
fn cache_insert(key: PathBuf, entry: FileCacheEntry) {
    file_cache().insert(key, entry);
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
        let fast_key = root.join(relative);

        if let Some(entry) = cache_get(&fast_key) {
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
                    let etag_val_str = etag_str.to_owned();
                    let slice = entry.data.slice(range_start as usize..=range_end as usize);
                    let mut resp = WebResponse::new(ResponseBody::from(slice));
                    *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
                    set_file_headers(resp.headers_mut(), &fast_key, range_len, &etag_val_str, modified_secs, location);
                    let mut cr = String::with_capacity(32);
                    cr.push_str("bytes "); cr.push_str(itoa::Buffer::new().format(range_start));
                    cr.push('-'); cr.push_str(itoa::Buffer::new().format(range_end));
                    cr.push('/'); cr.push_str(itoa::Buffer::new().format(file_size));
                    if let Ok(v) = HeaderValue::from_str(&cr) { resp.headers_mut().insert(CONTENT_RANGE, v); }
                    return resp;
                }
                // Range 解析失败（超出范围等），回落到下方常规路径
            }

            // 选择最优编码：br > gz > 原始
            let accept_enc = req_headers.get(ACCEPT_ENCODING).and_then(|v| v.to_str().ok()).unwrap_or("");
            let (body_bytes, enc) = if accept_enc.contains("br") {
                if let Some(br) = &entry.br {
                    (br.clone(), Some("br"))
                } else {
                    (entry.data.clone(), None)
                }
            } else if accept_enc.contains("gzip") {
                if let Some(gz) = &entry.gz {
                    (gz.clone(), Some("gzip"))
                } else {
                    (entry.data.clone(), None)
                }
            } else {
                (entry.data.clone(), None)
            };

            // 直接插入预构建的头，零 from_str 分配
            let mut resp = WebResponse::new(ResponseBody::from(body_bytes.clone()));
            let h = resp.headers_mut();
            h.insert(CONTENT_TYPE,   entry.hv_content_type.clone());
            // 压缩后 Content-Length 需重新计算
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
            // 超出条数上限时淘汰最旧 1/4
            {
                let l2 = file_cache();
                if l2.len() >= FILE_CACHE_MAX_ENTRIES {
                    let to_remove: Vec<_> = l2.iter()
                        .take(FILE_CACHE_MAX_ENTRIES / 4)
                        .map(|e| e.key().clone())
                        .collect();
                    for k in to_remove { l2.remove(&k); }
                }
            }
            let bytes = Bytes::from(data);
            let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let mime = mime_type_for(ext);

            // 写入缓存时预计算 gzip/brotli 压缩（仅对可压缩 mime 类型）
            // 后续请求缓存命中时直接返回预压缩内容，无需重复读文件和压缩
            let global = &ctx.state().cfg.load().global;
            let gzip_enabled = site.gzip.unwrap_or(global.gzip);
            let min_bytes = (global.gzip_min_length as u64) * 1024;
            let gzip_level = site.gzip_comp_level.unwrap_or(global.gzip_comp_level);
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
            let should_precompress = gzip_enabled && !already_compressed && compressible_mime
                && file_size >= min_bytes && file_size <= GZIP_MAX_INLINE;

            let (gz_bytes, br_bytes) = if should_precompress {
                let raw = bytes.clone();
                let raw2 = bytes.clone();
                let gz = tokio::task::spawn_blocking(move || gzip_compress(&raw, gzip_level))
                    .await.ok().and_then(|r| r.ok());
                let br = brotli_compress(&raw2).await.ok();
                (gz, br)
            } else {
                (None, None)
            };

            let modified_time = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(modified_secs);
            let http_date = httpdate::fmt_http_date(modified_time);
            let cc = location.cache_control.as_deref().unwrap_or_else(|| default_cache_control(ext));
            let mut cl_buf = itoa::Buffer::new();
            let entry = FileCacheEntry {
                data:               bytes.clone(),
                gz:                 gz_bytes,
                br:                 br_bytes,
                modified_secs,
                hv_content_type:    HeaderValue::from_str(mime).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
                hv_content_length:  HeaderValue::from_str(cl_buf.format(file_size)).unwrap_or_else(|_| HeaderValue::from_static("0")),
                hv_etag:            HeaderValue::from_str(&etag_val).unwrap_or_else(|_| HeaderValue::from_static("\"\"") ),
                hv_last_modified:   HeaderValue::from_str(&http_date).unwrap_or_else(|_| HeaderValue::from_static("Thu, 01 Jan 1970 00:00:00 GMT")),
                hv_cache_control:   HeaderValue::from_str(cc).unwrap_or_else(|_| HeaderValue::from_static("public, max-age=3600")),
            };
            // 获取请求的 Accept-Encoding，选择最优编码后写缓存并直接返回
            let accept_enc2 = req_headers.get(ACCEPT_ENCODING).and_then(|v| v.to_str().ok()).unwrap_or("");
            let (resp_bytes, enc_hv) = if accept_enc2.contains("br") {
                if let Some(b) = &entry.br { (b.clone(), Some(HeaderValue::from_static("br"))) }
                else { (entry.data.clone(), None) }
            } else if accept_enc2.contains("gzip") {
                if let Some(g) = &entry.gz { (g.clone(), Some(HeaderValue::from_static("gzip"))) }
                else { (entry.data.clone(), None) }
            } else {
                (entry.data.clone(), None)
            };
            let hv_ct = entry.hv_content_type.clone();
            let hv_cl = entry.hv_content_length.clone();
            let hv_et = entry.hv_etag.clone();
            let hv_lm = entry.hv_last_modified.clone();
            let hv_cc = entry.hv_cache_control.clone();
            // 同时插入 canonical key 和 fast_key，保证两条查询路径都能命中
            cache_insert(file_path.clone(), entry.clone());
            if file_path != fast_key { cache_insert(fast_key.clone(), entry); }
            let mut resp = WebResponse::new(ResponseBody::from(resp_bytes.clone()));
            let h = resp.headers_mut();
            h.insert(CONTENT_TYPE,   hv_ct);
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
                    let len = range_len as usize;
                    match tokio::task::spawn_blocking(move || {
                        crate::handler::sendfile::pread_exact(&arc_fd, range_start, len)
                    }).await {
                        Ok(Ok(buf)) => ResponseBody::from(buf),
                        _ => return make_error(StatusCode::INTERNAL_SERVER_ERROR, ""),
                    }
                } else {
                    // 超大 Range（> 4MB）：流式传输避免内存峓发
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
            let len = range_len as usize;
            match tokio::task::spawn_blocking(move || {
                crate::handler::sendfile::pread_exact(&arc_fd, range_start, len)
            }).await {
                Ok(Ok(buf)) => ResponseBody::from(buf),
                _ => return make_error(StatusCode::INTERNAL_SERVER_ERROR, ""),
            }
        } else {
            // 超大 Range（> 4MB）：流式传输避免内存峓发
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


/// pread 分块流式传输：共享 fd + spawn_blocking pread，无 seek，无竞争
/// 适用于大文件全量传输（offset=0, len=file_size）和大 Range 传输
fn stream_file_response_pread(
    fd: std::sync::Arc<std::fs::File>,
    file_path: &Path,
    content_len: u64,
    offset: u64,
    len: u64,
    etag_val: &str,
    modified_secs: u64,
    location: &LocationConfig,
) -> WebResponse {
    let stream = crate::handler::sendfile::pread_stream(fd, offset, len);
    let body = ResponseBody::box_stream(stream);
    let mut resp = WebResponse::new(body);
    set_file_headers(resp.headers_mut(), file_path, content_len, etag_val, modified_secs, location);
    resp
}

/// gzip 压缩（flate2，仅用于小文件；调用方需通过 spawn_blocking 调用）
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
