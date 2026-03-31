//! 缓存优化中间件
//! 负责：静态文件 ETag/Last-Modified 验证、Cache-Control 注入、PHP 页面 s-maxage 支持

use std::time::{SystemTime, UNIX_EPOCH};

/// 编译期 MIME 类型表（phf 完美哈希，O(1) 查找）
static MIME_MAP: phf::Map<&'static str, &'static str> = phf::phf_map! {
    "html"  => "text/html; charset=utf-8",
    "htm"   => "text/html; charset=utf-8",
    "css"   => "text/css; charset=utf-8",
    "js"    => "application/javascript; charset=utf-8",
    "mjs"   => "application/javascript; charset=utf-8",
    "json"  => "application/json; charset=utf-8",
    "xml"   => "application/xml; charset=utf-8",
    "txt"   => "text/plain; charset=utf-8",
    "png"   => "image/png",
    "jpg"   => "image/jpeg",
    "jpeg"  => "image/jpeg",
    "gif"   => "image/gif",
    "webp"  => "image/webp",
    "svg"   => "image/svg+xml",
    "ico"   => "image/x-icon",
    "woff"  => "font/woff",
    "woff2" => "font/woff2",
    "ttf"   => "font/ttf",
    "otf"   => "font/otf",
    "mp4"   => "video/mp4",
    "webm"  => "video/webm",
    "pdf"   => "application/pdf",
    "zip"   => "application/zip",
    "wasm"  => "application/wasm",
    "avif"  => "image/avif",
    "mp3"   => "audio/mpeg",
    "ogg"   => "audio/ogg",
    "flac"  => "audio/flac",
    "ts"    => "application/typescript; charset=utf-8",
};

/// 编译期缓存策略表（长缓存扩展名）
static LONG_CACHE: phf::Set<&'static str> = phf::phf_set! {
    "js", "mjs", "css", "woff", "woff2", "ttf", "otf",
    "png", "jpg", "jpeg", "gif", "webp", "svg", "ico",
    "wasm", "avif", "mp3", "ogg", "flac", "mp4", "webm",
};

/// 不缓存扩展名（HTML 每次验证）
static NO_CACHE: phf::Set<&'static str> = phf::phf_set! {
    "html", "htm",
};

/// 根据文件元数据生成 ETag（基于 mtime + size 的弱 ETag）
/// 使用栈上 buffer 避免 format! 堆分配
pub fn generate_etag(size: u64, modified_secs: u64) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(32);
    let _ = write!(s, "W/\"{:x}-{:x}\"", size, modified_secs);
    s
}

/// 将 SystemTime 转换为 Unix 时间戳（秒）
pub fn to_unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 检查客户端缓存是否仍然有效（ETag 或 Last-Modified 验证）
///
/// 参数：
/// - `if_none_match`: 客户端发送的 `If-None-Match` 头值
/// - `if_modified_since_secs`: 客户端发送的 `If-Modified-Since` 对应的时间戳（秒）
/// - `etag`: 当前资源的 ETag
/// - `last_modified_secs`: 当前资源的最后修改时间戳（秒）
///
/// 返回 `true` 表示客户端缓存有效，应返回 304 Not Modified
pub fn is_cache_valid(
    if_none_match: Option<&str>,
    if_modified_since_secs: Option<u64>,
    etag: &str,
    last_modified_secs: u64,
) -> bool {
    // ETag 匹配（优先）
    if let Some(inm) = if_none_match {
        return inm == etag || inm == "*";
    }
    // Last-Modified 匹配
    if let Some(ims) = if_modified_since_secs {
        return last_modified_secs <= ims;
    }
    false
}

/// 根据文件扩展名推断 MIME 类型（phf 完美哈希表，O(1) 查找）
pub fn mime_type_for(extension: &str) -> &'static str {
    // 扩展名一般很短（≤ 5 字节），用栈分配避免堆分配
    let mut buf = [0u8; 8];
    let ext = extension.as_bytes();
    if ext.len() <= 8 {
        for (i, &b) in ext.iter().enumerate() {
            buf[i] = b.to_ascii_lowercase();
        }
        let lower = std::str::from_utf8(&buf[..ext.len()]).unwrap_or("");
        if let Some(&mime) = MIME_MAP.get(lower) { return mime; }
    }
    "application/octet-stream"
}

/// 为静态资源生成默认 Cache-Control 头（phf 完美哈希表，O(1) 查找）
///
/// - 图片、字体、JS、CSS → 长缓存（7天）
/// - HTML → 不缓存（no-cache，让浏览器每次验证）
/// - 其他 → 短缓存（1小时）
pub fn default_cache_control(extension: &str) -> &'static str {
    let mut buf = [0u8; 8];
    let ext = extension.as_bytes();
    if ext.len() <= 8 {
        for (i, &b) in ext.iter().enumerate() {
            buf[i] = b.to_ascii_lowercase();
        }
        let lower = std::str::from_utf8(&buf[..ext.len()]).unwrap_or("");
        if LONG_CACHE.contains(lower) { return "public, max-age=604800, immutable"; }
        if NO_CACHE.contains(lower)   { return "no-cache, must-revalidate"; }
    }
    "public, max-age=3600"
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_etag_generation_deterministic() {
        let e1 = generate_etag(1024, 1700000000);
        let e2 = generate_etag(1024, 1700000000);
        assert_eq!(e1, e2);
    }

    #[test]
    fn test_etag_changes_on_modification() {
        let e1 = generate_etag(1024, 1700000000);
        let e2 = generate_etag(1024, 1700000001);
        assert_ne!(e1, e2);
    }

    #[test]
    fn test_cache_valid_etag_match() {
        let etag = generate_etag(512, 100);
        assert!(is_cache_valid(Some(&etag), None, &etag, 100));
    }

    #[test]
    fn test_cache_valid_wildcard_etag() {
        assert!(is_cache_valid(Some("*"), None, "W/\"any\"", 0));
    }

    #[test]
    fn test_cache_invalid_etag_mismatch() {
        assert!(!is_cache_valid(Some("W/\"old\""), None, "W/\"new\"", 0));
    }

    #[test]
    fn test_cache_valid_last_modified() {
        // 客户端 ims = 1000，资源 mtime = 1000 → 缓存有效
        assert!(is_cache_valid(None, Some(1000), "W/\"x\"", 1000));
        // 资源 mtime = 999 也有效（未变更）
        assert!(is_cache_valid(None, Some(1000), "W/\"x\"", 999));
        // 资源 mtime = 1001 → 缓存失效
        assert!(!is_cache_valid(None, Some(1000), "W/\"x\"", 1001));
    }

    #[test]
    fn test_mime_type_html() {
        assert_eq!(mime_type_for("html"), "text/html; charset=utf-8");
        assert_eq!(mime_type_for("HTML"), "text/html; charset=utf-8");
    }

    #[test]
    fn test_mime_type_unknown() {
        assert_eq!(mime_type_for("xyz"), "application/octet-stream");
    }

    #[test]
    fn test_default_cache_control_js() {
        assert!(default_cache_control("js").contains("immutable"));
    }

    #[test]
    fn test_default_cache_control_html() {
        assert!(default_cache_control("html").contains("no-cache"));
    }
}
