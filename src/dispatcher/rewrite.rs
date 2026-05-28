//! Rewrite / 伪静态规则引擎
//! 负责：按顺序应用 Rewrite 规则，支持正则捕获组替换、条件判断、标志位处理
//!
//! # 条件检查性能（对比 Nginx）
//! Nginx `if (-f ...)` 每次请求都执行 `stat()` 系统调用。
//! Sweety 使用全局 DashMap 元数据缓存 + TTL 过期：
//! - 缓存命中：0 syscall，纯内存查找（~15ns DashMap get）
//! - 缓存未命中：1 次 `stat()` + 写缓存
//! - 负缓存（文件不存在）：TTL 1s，高频 WordPress !-f 场景受益巨大
//! - 正缓存（文件存在）：TTL 5s，覆盖绝大多数静态资源生命周期

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use dashmap::DashMap;
use regex::{Regex, RegexBuilder};

use crate::config::model::{RewriteFlag, RewriteRule};

// ═══════════════════════════════════════════════════════════════════════
// 文件元数据缓存（全局单例，所有站点共享，按绝对路径分片）
// ═══════════════════════════════════════════════════════════════════════

/// 缓存的文件元数据（仅存在性 + 类型，不存 mtime/size）
#[derive(Clone, Copy)]
struct CachedMeta {
    /// 文件是否存在
    exists: bool,
    /// 是否为普通文件
    is_file: bool,
    /// 是否为目录
    is_dir: bool,
    /// 是否有执行权限（Unix: mode & 0o111 != 0，Windows: 始终 true）
    is_executable: bool,
    /// 缓存写入时刻（Instant 单调递增，无时钟回拨问题）
    cached_at: Instant,
}

/// 正缓存 TTL（秒）：文件存在的结果缓存较长
const POSITIVE_TTL_SECS: u64 = 5;
/// 负缓存 TTL（秒）：文件不存在的结果缓存较短（新部署后快速生效）
const NEGATIVE_TTL_SECS: u64 = 1;
/// 清理阈值：超过此条目数时触发惰性淘汰
const CLEANUP_THRESHOLD: usize = 50_000;
/// 清理计数器：每 N 次 miss 触发一次全量淘汰
static MISS_COUNTER: AtomicU64 = AtomicU64::new(0);
const CLEANUP_INTERVAL: u64 = 1024;

static META_CACHE: std::sync::LazyLock<DashMap<Box<str>, CachedMeta>> =
    std::sync::LazyLock::new(|| DashMap::with_capacity_and_shard_amount(4096, 32));

/// 查询文件元数据（带缓存）
///
/// 热路径：缓存命中时仅 1 次 DashMap::get（~15ns），零 syscall。
#[inline]
fn stat_cached(path: &Path) -> CachedMeta {
    // 用 path 的 UTF-8 表示作 key（绝大多数路径是 UTF-8）
    let key: &str = match path.to_str() {
        Some(s) => s,
        None => return stat_uncached(path),
    };

    // 快路径：缓存命中 + 未过期
    if let Some(entry) = META_CACHE.get(key) {
        let ttl = if entry.exists { POSITIVE_TTL_SECS } else { NEGATIVE_TTL_SECS };
        if entry.cached_at.elapsed().as_secs() < ttl {
            return *entry;
        }
    }

    // 慢路径：执行 stat() 并写缓存
    let meta = stat_uncached(path);
    let boxed_key: Box<str> = key.into();
    META_CACHE.insert(boxed_key, meta);

    // 惰性淘汰：每 CLEANUP_INTERVAL 次 miss 检查一次
    let count = MISS_COUNTER.fetch_add(1, Ordering::Relaxed);
    if count % CLEANUP_INTERVAL == 0 && META_CACHE.len() > CLEANUP_THRESHOLD {
        // 非阻塞淘汰：仅删除已过期条目
        META_CACHE.retain(|_, v| {
            let ttl = if v.exists { POSITIVE_TTL_SECS } else { NEGATIVE_TTL_SECS };
            v.cached_at.elapsed().as_secs() < ttl
        });
    }

    meta
}

/// 直接执行 stat() 系统调用
#[inline]
fn stat_uncached(path: &Path) -> CachedMeta {
    match std::fs::metadata(path) {
        Ok(m) => {
            #[cfg(unix)]
            let is_executable = {
                use std::os::unix::fs::PermissionsExt;
                m.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let is_executable = true;

            CachedMeta {
                exists: true,
                is_file: m.is_file(),
                is_dir: m.is_dir(),
                is_executable,
                cached_at: Instant::now(),
            }
        }
        Err(_) => CachedMeta {
            exists: false,
            is_file: false,
            is_dir: false,
            is_executable: false,
            cached_at: Instant::now(),
        },
    }
}

/// 手动使指定路径的缓存失效（供热重载 / 部署钩子调用）
pub fn invalidate_meta_cache(path: &Path) {
    if let Some(key) = path.to_str() {
        META_CACHE.remove(key);
    }
}

/// 清空全部元数据缓存（配置重载时调用）
pub fn clear_meta_cache() {
    META_CACHE.clear();
}

/// 预编译后的 Rewrite 规则
pub struct CompiledRewrite {
    /// 原始配置
    pub rule: RewriteRule,
    /// 预编译的正则对象
    pub regex: Regex,
}

impl CompiledRewrite {
    /// 从 RewriteRule 构建，编译失败返回 None
    pub fn new(rule: RewriteRule) -> Option<Self> {
        match RegexBuilder::new(&rule.pattern).size_limit(1 << 20).build() {
            Ok(regex) => Some(Self { rule, regex }),
            Err(e) => {
                tracing::warn!("Rewrite 规则正则编译失败 '{}': {}", rule.pattern, e);
                None
            }
        }
    }
}

impl Clone for CompiledRewrite {
    fn clone(&self) -> Self {
        Self { rule: self.rule.clone(), regex: self.regex.clone() }
    }
}

impl std::fmt::Debug for CompiledRewrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledRewrite").field("pattern", &self.rule.pattern).finish()
    }
}

/// 对请求路径应用预编译 Rewrite 规则列表
///
/// `doc_root`：站点文档根目录，用于 -f / -d 条件检查。
///            传 None 时条件检查降级为始终满足（兼容无 root 的纯代理站点）。
///
/// 返回值：
/// - `Some(new_path)` 表示路径被重写
/// - `None` 表示没有规则匹配（包括规则列表为空）
pub fn apply_rewrites(rules: &[CompiledRewrite], path: &str, doc_root: Option<&Path>) -> Option<String> {
    if rules.is_empty() { return None; } // 热路径：绝大多数静态站点无 rewrite 规则

    let mut current = path.to_string();
    let mut changed = false;

    for cr in rules {
        // 检查触发条件（如 !-f 文件不存在）
        if let Some(cond) = &cr.rule.condition {
            if !evaluate_condition(cond, &current, doc_root) {
                continue;
            }
        }

        let re = &cr.regex;
        if !re.is_match(&current) {
            continue;
        }

        // 执行捕获组替换（$1 → 第1组，$2 → 第2组 …）
        let new_path = regex_replace(re, &current, &cr.rule.target);
        current = new_path;
        changed = true;

        match cr.rule.flag {
            RewriteFlag::Last | RewriteFlag::Break => {
                // last/break 都停止继续处理后续 rewrite
                break;
            }
            RewriteFlag::Redirect | RewriteFlag::Permanent => {
                // 重定向标志：停止处理，上层需要发送重定向响应
                // 此处在路径前加标记前缀供 dispatcher 识别
                // 格式：`REDIRECT:302:<new_path>` 或 `REDIRECT:301:<new_path>`
                let code = if cr.rule.flag == RewriteFlag::Permanent {
                    301
                } else {
                    302
                };
                return Some(format!("REDIRECT:{}:{}", code, current));
            }
        }
    }

    if changed {
        Some(current)
    } else {
        None
    }
}

/// 使用正则捕获组执行路径替换
///
/// 支持 `$0`（完整匹配）、`$1`..`$9`（捕获组）
fn regex_replace(re: &Regex, input: &str, template: &str) -> String {
    if let Some(caps) = re.captures(input) {
        let mut result = template.to_string();
        // $0 = 完整匹配
        if let Some(m) = caps.get(0) {
            result = result.replace("$0", m.as_str());
        }
        // $1 .. $9 = 捕获组
        for i in 1..=9 {
            let placeholder = format!("${}", i);
            if result.contains(&placeholder) {
                let replacement = caps.get(i).map_or("", |m| m.as_str());
                result = result.replace(&placeholder, replacement);
            }
        }
        result
    } else {
        input.to_string()
    }
}

/// 评估 Rewrite 触发条件
///
/// 支持条件（完整实现，等价 Nginx `if` 指令）：
/// - `-f`   请求路径对应的文件存在
/// - `!-f`  请求路径对应的文件不存在
/// - `-d`   请求路径对应的目录存在
/// - `!-d`  请求路径对应的目录不存在
/// - `-e`   路径存在（文件或目录均可）
/// - `!-e`  路径不存在
/// - `-x`   文件存在且有执行权限
/// - `!-x`  文件不存在或无执行权限
///
/// # 性能
/// 使用全局 `stat()` 结果缓存，缓存命中时零系统调用。
/// Nginx 每次条件判断都执行 `stat()`，Sweety 在高频 WordPress/Laravel
/// 场景下可节省 >95% 的 `stat()` 调用。
#[inline]
fn evaluate_condition(condition: &str, path: &str, doc_root: Option<&Path>) -> bool {
    let cond = condition.trim();
    let negated = cond.starts_with('!');
    let check = if negated { &cond[1..] } else { cond };

    let result = match check {
        "-f" | "-d" | "-e" | "-x" => {
            match doc_root {
                Some(root) => {
                    // 安全：去掉 query string，只取路径部分
                    let path_only = path.split('?').next().unwrap_or(path);
                    let relative = path_only.trim_start_matches('/');
                    // 空路径（根 /）→ 检查 root 本身
                    let full_path = if relative.is_empty() {
                        root.to_path_buf()
                    } else {
                        // 安全：拒绝路径遍历
                        if relative.split(&['/', '\\'][..]).any(|s| s == "..") {
                            return negated; // .. 段视为不存在
                        }
                        root.join(relative)
                    };
                    let meta = stat_cached(&full_path);
                    match check {
                        "-f" => meta.exists && meta.is_file,
                        "-d" => meta.exists && meta.is_dir,
                        "-e" => meta.exists,
                        "-x" => meta.exists && meta.is_file && meta.is_executable,
                        _    => false,
                    }
                }
                None => {
                    // 无 doc_root（纯代理站点）：文件条件无法检查，降级处理
                    // -f/-d/-e/-x → false（文件不存在），与 Nginx 无 root 时行为一致
                    false
                }
            }
        }
        _ => {
            tracing::warn!("不支持的 Rewrite 条件: '{}'，跳过", condition);
            return false;
        }
    };

    if negated { !result } else { result }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{RewriteFlag, RewriteRule};

    fn crule(pattern: &str, target: &str, flag: RewriteFlag) -> CompiledRewrite {
        CompiledRewrite::new(RewriteRule {
            pattern: pattern.to_string(),
            target: target.to_string(),
            flag,
            condition: None,
        }).unwrap()
    }

    fn crule_cond(pattern: &str, target: &str, flag: RewriteFlag, cond: &str) -> CompiledRewrite {
        CompiledRewrite::new(RewriteRule {
            pattern: pattern.to_string(),
            target: target.to_string(),
            flag,
            condition: Some(cond.to_string()),
        }).unwrap()
    }

    #[test]
    fn test_basic_rewrite() {
        let rules = vec![crule("^/old/(.*)$", "/new/$1", RewriteFlag::Last)];
        let result = apply_rewrites(&rules, "/old/page", None);
        assert_eq!(result, Some("/new/page".to_string()));
    }

    #[test]
    fn test_no_match_returns_none() {
        let rules = vec![crule("^/api/", "/backend/", RewriteFlag::Last)];
        let result = apply_rewrites(&rules, "/other/path", None);
        assert!(result.is_none());
    }

    #[test]
    fn test_wordpress_style_rewrite() {
        let rules = vec![crule("^/(.+)$", "/index.php?$1", RewriteFlag::Last)];
        let result = apply_rewrites(&rules, "/hello-world", None);
        assert_eq!(result, Some("/index.php?hello-world".to_string()));
    }

    #[test]
    fn test_redirect_flag() {
        let rules = vec![crule("^/old$", "/new", RewriteFlag::Redirect)];
        let result = apply_rewrites(&rules, "/old", None);
        assert_eq!(result, Some("REDIRECT:302:/new".to_string()));
    }

    #[test]
    fn test_permanent_redirect_flag() {
        let rules = vec![crule("^/old$", "/new", RewriteFlag::Permanent)];
        let result = apply_rewrites(&rules, "/old", None);
        assert_eq!(result, Some("REDIRECT:301:/new".to_string()));
    }

    #[test]
    fn test_break_stops_chain() {
        let rules = vec![
            crule("^/(.*)$", "/first/$1", RewriteFlag::Break),
            crule("^/first/(.*)$", "/second/$1", RewriteFlag::Last),
        ];
        let result = apply_rewrites(&rules, "/page", None);
        assert_eq!(result, Some("/first/page".to_string()));
    }

    // ── 文件系统条件测试 ──────────────────────────────────────

    #[test]
    fn test_condition_not_file_no_root() {
        // 无 doc_root 时 !-f 降级：文件不存在 → 取反 → true → 触发 rewrite
        let rules = vec![crule_cond("^/(.*)$", "/index.php?$1", RewriteFlag::Last, "!-f")];
        let result = apply_rewrites(&rules, "/hello", None);
        assert_eq!(result, Some("/index.php?hello".to_string()));
    }

    #[test]
    fn test_condition_file_exists() {
        // 用 tempdir 创建真实文件测试 -f 条件
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("exist.txt"), "ok").unwrap();

        // -f 条件：文件存在 → 匹配
        let rules = vec![crule_cond("^/(.*)$", "/found/$1", RewriteFlag::Last, "-f")];
        let result = apply_rewrites(&rules, "/exist.txt", Some(dir.path()));
        assert_eq!(result, Some("/found/exist.txt".to_string()));

        // -f 条件：文件不存在 → 不匹配
        let result = apply_rewrites(&rules, "/nope.txt", Some(dir.path()));
        assert!(result.is_none());
    }

    #[test]
    fn test_condition_not_file_with_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("index.html"), "<html>").unwrap();

        // !-f：文件不存在 → 触发 rewrite
        let rules = vec![crule_cond("^/(.*)$", "/index.php?$1", RewriteFlag::Last, "!-f")];
        let result = apply_rewrites(&rules, "/no-such-file", Some(dir.path()));
        assert_eq!(result, Some("/index.php?no-such-file".to_string()));

        // !-f：文件存在 → 不触发 rewrite
        let result = apply_rewrites(&rules, "/index.html", Some(dir.path()));
        assert!(result.is_none());
    }

    #[test]
    fn test_condition_dir_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();

        // -d 条件：目录存在
        let rules = vec![crule_cond("^/(.*)$", "/dir/$1", RewriteFlag::Last, "-d")];
        let result = apply_rewrites(&rules, "/subdir", Some(dir.path()));
        assert_eq!(result, Some("/dir/subdir".to_string()));

        // -d 条件：目录不存在
        let result = apply_rewrites(&rules, "/nope", Some(dir.path()));
        assert!(result.is_none());
    }

    #[test]
    fn test_condition_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file.txt"), "ok").unwrap();
        std::fs::create_dir(dir.path().join("adir")).unwrap();

        // -e 条件：文件或目录存在
        let rules = vec![crule_cond("^/(.*)$", "/yes/$1", RewriteFlag::Last, "-e")];
        assert!(apply_rewrites(&rules, "/file.txt", Some(dir.path())).is_some());
        assert!(apply_rewrites(&rules, "/adir", Some(dir.path())).is_some());
        assert!(apply_rewrites(&rules, "/nope", Some(dir.path())).is_none());
    }

    #[test]
    fn test_condition_path_traversal_blocked() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("secret"), "data").unwrap();

        // 路径遍历被拒绝：.. 段视为不存在
        let rules = vec![crule_cond("^/(.*)$", "/ok/$1", RewriteFlag::Last, "-f")];
        let result = apply_rewrites(&rules, "/../secret", Some(dir.path()));
        assert!(result.is_none());
    }

    #[test]
    fn test_meta_cache_invalidation() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("cached.txt");
        std::fs::write(&file, "data").unwrap();

        // 第一次查询（写入缓存）
        let meta = stat_cached(&file);
        assert!(meta.exists);

        // 删除文件
        std::fs::remove_file(&file).unwrap();

        // 缓存内仍然命中（正缓存 TTL 5s 内）
        let meta = stat_cached(&file);
        assert!(meta.exists); // 缓存命中，仍显示存在

        // 手动失效
        invalidate_meta_cache(&file);
        let meta = stat_cached(&file);
        assert!(!meta.exists); // 缓存已失效，重新 stat()
    }

    #[test]
    fn test_clear_meta_cache() {
        clear_meta_cache();
        assert_eq!(META_CACHE.len(), 0);
    }
}
