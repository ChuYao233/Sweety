//! 静态文件内存缓存（FileCacheEntry）、fd 缓存（FdCacheEntry）及 notify watcher

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use bytes::Bytes;
use dashmap::DashMap;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use sweety_web::http::header::HeaderValue;

// ─────────────────────────────────────────────
// 常量 & 可配置参数
// ─────────────────────────────────────────────

pub(super) const GZIP_MAX_INLINE: u64    = 1024 * 1024;      // 1 MB
pub(super) const FILE_CACHE_MAX_BYTES: u64 = 64 * 1024;      // 64 KB（单文件缓存上限）

/// 运行时缓存参数（由 `init_cache_limits` 从 GlobalConfig 初始化）
static CACHE_LIMITS: OnceLock<CacheLimits> = OnceLock::new();

struct CacheLimits {
    file_cache_total_bytes: usize,
    file_cache_max_entries: usize,
    fd_cache_max_entries: usize,
}

impl Default for CacheLimits {
    fn default() -> Self {
        Self {
            file_cache_total_bytes: 512 * 1024 * 1024, // 512 MB
            file_cache_max_entries: 200000,
            fd_cache_max_entries: 25000,
        }
    }
}

fn limits() -> &'static CacheLimits {
    CACHE_LIMITS.get_or_init(CacheLimits::default)
}

/// 从全局配置初始化缓存限制（服务器启动时调用一次）
///
/// 等价 Nginx:
/// - `open_file_cache max=200000 inactive=60s;`
/// - `open_file_cache_min_uses 2;`
/// - fd 条目数 = max / 8（Nginx 内部近似比例）
pub fn init_cache_limits(max_entries: usize, total_mb: usize) {
    let _ = CACHE_LIMITS.set(CacheLimits {
        file_cache_total_bytes: total_mb.max(16) * 1024 * 1024,
        file_cache_max_entries: max_entries.max(256),
        fd_cache_max_entries: (max_entries / 8).max(256),
    });
}

// ─────────────────────────────────────────────
// min_uses 访问计数器（等价 Nginx open_file_cache_min_uses）
// 防止一次性爬虫/扫描器污染热缓存
// ─────────────────────────────────────────────

/// 全局默认 min_uses 阈值（访问 N 次后才写入内容缓存）
const MIN_USES_THRESHOLD: u8 = 2;

static ACCESS_COUNTER: OnceLock<DashMap<Arc<str>, u8>> = OnceLock::new();

fn access_counter() -> &'static DashMap<Arc<str>, u8> {
    ACCESS_COUNTER.get_or_init(|| DashMap::with_capacity(4096))
}

/// 记录一次访问，返回 true 表示已达到 min_uses 阈值，允许写入缓存
#[inline]
pub(super) fn access_count_check(key: &Arc<str>) -> bool {
    let mut entry = access_counter().entry(key.clone()).or_insert(0);
    let count = *entry;
    if count >= MIN_USES_THRESHOLD {
        return true;
    }
    *entry = count.saturating_add(1);
    count + 1 >= MIN_USES_THRESHOLD
}

/// 定期清理过期访问计数器（防内存泄漏，与 fd_cache 清理一同调用）
pub fn cleanup_access_counter() {
    let counter = access_counter();
    if counter.len() > 500_000 {
        // 简单随机淘汰 1/4（低开销，无需精确 LRU）
        let to_remove: Vec<_> = counter.iter()
            .take(counter.len() / 4)
            .map(|e| e.key().clone())
            .collect();
        for k in to_remove { counter.remove(&k); }
    }
}

// ─────────────────────────────────────────────
// 缓存结构体
// ─────────────────────────────────────────────

/// 文件内存缓存条目（预缓存所有响应头 HeaderValue，缓存命中时零分配）
#[derive(Clone)]
pub(super) struct FileCacheEntry {
    pub(super) data:          Bytes,
    pub(super) gz:            Option<Bytes>,
    pub(super) br:            Option<Bytes>,
    pub(super) zst:           Option<Bytes>,
    pub(super) modified_secs: u64,
    pub(super) hv_content_type:   HeaderValue,
    pub(super) hv_content_length: HeaderValue,
    pub(super) hv_etag:           HeaderValue,
    pub(super) hv_last_modified:  HeaderValue,
    pub(super) hv_cache_control:  HeaderValue,
}

/// 大文件 fd 缓存条目：缓存 fd + stat，避免重复 open syscall
#[derive(Clone)]
pub(super) struct FdCacheEntry {
    #[allow(dead_code)]
    pub(super) file_size:     u64,
    #[allow(dead_code)]
    pub(super) modified_secs: u64,
    pub(super) fd:            Arc<std::fs::File>,
}

// ─────────────────────────────────────────────
// 全局缓存
// ─────────────────────────────────────────────

static FILE_CACHE: OnceLock<DashMap<Arc<str>, FileCacheEntry>> = OnceLock::new();
static FD_CACHE:   OnceLock<DashMap<Arc<str>, FdCacheEntry>>   = OnceLock::new();

pub(super) fn file_cache() -> &'static DashMap<Arc<str>, FileCacheEntry> {
    FILE_CACHE.get_or_init(|| DashMap::with_capacity(limits().file_cache_max_entries.min(4096)))
}

pub(super) fn fd_cache() -> &'static DashMap<Arc<str>, FdCacheEntry> {
    FD_CACHE.get_or_init(|| DashMap::with_capacity(limits().fd_cache_max_entries.min(1024)))
}

// ─────────────────────────────────────────────
// key 构建辅助
// ─────────────────────────────────────────────

#[inline]
pub(super) fn make_cache_key(root: &Path, relative: &str) -> Arc<str> {
    let root_str = root.to_str().unwrap_or("");
    let mut s = String::with_capacity(root_str.len() + 1 + relative.len());
    s.push_str(root_str);
    s.push('/');
    s.push_str(relative);
    Arc::from(s.as_str())
}

#[inline]
pub(super) fn make_cache_key_from_path(path: &Path) -> Arc<str> {
    Arc::from(path.to_str().unwrap_or(""))
}

/// 零分配缓存查询（栈缓冲拼接 key，不分配堆内存）
#[inline]
pub(super) fn cache_get_fast(root: &Path, relative: &str) -> Option<FileCacheEntry> {
    let root_str = root.to_str().unwrap_or("");
    let mut buf = [0u8; 512];
    let root_bytes = root_str.as_bytes();
    let rel_bytes  = relative.as_bytes();
    let total = root_bytes.len() + 1 + rel_bytes.len();
    if total <= 512 {
        buf[..root_bytes.len()].copy_from_slice(root_bytes);
        buf[root_bytes.len()] = b'/';
        buf[root_bytes.len() + 1..total].copy_from_slice(rel_bytes);
        if let Ok(key) = std::str::from_utf8(&buf[..total]) {
            file_cache().get(key).map(|e| e.clone())
        } else {
            None
        }
    } else {
        let key = make_cache_key(root, relative);
        file_cache().get(key.as_ref()).map(|e| e.clone())
    }
}

// ─────────────────────────────────────────────
// fd 缓存操作
// ─────────────────────────────────────────────

#[inline]
pub(super) fn fd_cache_get(key: &str) -> Option<FdCacheEntry> {
    fd_cache().get(key).map(|e| e.clone())
}

#[inline]
pub(super) async fn fd_cache_get_or_open_arc(
    path: &PathBuf,
    file_size: u64,
    modified_secs: u64,
) -> Option<Arc<std::fs::File>> {
    let key: Arc<str> = Arc::from(path.to_str().unwrap_or(""));
    if let Some(entry) = fd_cache_get(&key) {
        return Some(entry.fd);
    }
    match std::fs::File::open(path) {
        Ok(f) => {
            let arc = Arc::new(f);
            fd_cache_insert_arc(key, arc.clone(), file_size, modified_secs);
            Some(arc)
        }
        Err(_) => None,
    }
}

#[inline]
pub(super) fn fd_cache_insert_arc(
    key: Arc<str>,
    fd: Arc<std::fs::File>,
    file_size: u64,
    modified_secs: u64,
) {
    let cache = fd_cache();
    let max = limits().fd_cache_max_entries;
    if cache.len() >= max {
        let to_remove: Vec<_> = cache.iter()
            .take(max / 4)
            .map(|e| e.key().clone())
            .collect();
        for k in to_remove { cache.remove(&k); }
    }
    cache.insert(key, FdCacheEntry { file_size, modified_secs, fd });
}

// ─────────────────────────────────────────────
// 文件缓存操作
// ─────────────────────────────────────────────

pub(super) fn cache_insert(key: Arc<str>, entry: FileCacheEntry) {
    let cache = file_cache();
    let lim = limits();
    let total: usize = cache.iter().map(|e| {
        e.data.len()
            + e.gz.as_ref().map(|b| b.len()).unwrap_or(0)
            + e.br.as_ref().map(|b| b.len()).unwrap_or(0)
            + e.zst.as_ref().map(|b| b.len()).unwrap_or(0)
    }).sum();
    if total + entry.data.len() > lim.file_cache_total_bytes || cache.len() >= lim.file_cache_max_entries {
        let to_remove: Vec<_> = cache.iter()
            .take(lim.file_cache_max_entries / 4)
            .map(|e| e.key().clone())
            .collect();
        for k in to_remove { cache.remove(&k); }
    }
    cache.insert(key, entry);
}

// ─────────────────────────────────────────────
// notify watcher
// ─────────────────────────────────────────────

/// 启动文件变更监听：文件修改/删除时直接淘汰 FILE_CACHE 对应条目
pub fn start_file_cache_watcher(roots: Vec<PathBuf>) -> Option<RecommendedWatcher> {
    if roots.is_empty() { return None; }

    let watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let Ok(event) = res else { return };
        if !matches!(event.kind,
            EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
        ) { return; }

        let cache = file_cache();
        for path in &event.paths {
            if let Some(key) = path.to_str() {
                if cache.remove(key).is_some() {
                    tracing::debug!("文件缓存已淡化: {}", path.display());
                }
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
