//! Location 路径匹配模块
//! 负责：在单个站点内按优先级匹配请求路径到 LocationConfig
//! 优先级规则参照 Nginx：精确 > 前缀优先 > 正则 > 普通前缀
//!
//! 性能关键点：正则对象在站点初始化时预编译，请求时直接匹配，避免每请求 Regex::new()

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

use regex::Regex;

use crate::config::model::LocationConfig;
use crate::middleware::access_control::CompiledAccessRule;

/// Location 类型（在构建时一次解析，请求时直接匹配不再扫描字符串）
#[derive(Debug, Clone)]
pub enum LocationKind {
    /// 精确匹配：`= /path`
    Exact(String),
    /// 前缀优先：`^~ /prefix`
    PrefixPriority(String),
    /// 正则（已预编译）
    Regex(Regex),
    /// 普通前缀
    Prefix,
}

/// 预编译后的 Location（所有正则在站点启动时一次编译）
pub struct CompiledLocation {
    /// 原始配置
    pub config: LocationConfig,
    /// 类型标记（请求时直接匹配，不再扫描 path 字段）
    pub kind: LocationKind,
    /// 兼容旧接口，保留 regex 字段（仅正则类型有值）
    pub regex: Option<Regex>,
    /// per-location 并发连接计数器（limit_conn 功能用）
    pub conn_count: Arc<AtomicUsize>,
    /// per-location 并发连接上限（从 config.limit_conn 拷贝过来，错误时不再访问 config）
    pub limit_conn: usize,
    /// 预编译的 IP 访问控制规则（启动时编译，运行时零分配）
    pub access_rules: Vec<CompiledAccessRule>,
}

impl Clone for CompiledLocation {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            kind: self.kind.clone(),
            regex: self.regex.clone(),
            conn_count: Arc::clone(&self.conn_count),
            limit_conn: self.limit_conn,
            access_rules: self.access_rules.clone(),
        }
    }
}

impl std::fmt::Debug for CompiledLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledLocation")
            .field("path", &self.config.path)
            .field("has_regex", &self.regex.is_some())
            .finish()
    }
}

impl CompiledLocation {
    /// 从 LocationConfig 构建，预编译正则并确定类型
    pub fn new(cfg: LocationConfig) -> Self {
        let (kind, regex) = if let Some(p) = cfg.path.strip_prefix("~* ") {
            let re = Regex::new(&format!("(?i){}", p)).ok();
            let k = re.clone().map(LocationKind::Regex).unwrap_or(LocationKind::Prefix);
            (k, re)
        } else if let Some(p) = cfg.path.strip_prefix("~ ") {
            let re = Regex::new(p).ok();
            let k = re.clone().map(LocationKind::Regex).unwrap_or(LocationKind::Prefix);
            (k, re)
        } else if let Some(p) = cfg.path.strip_prefix("= ") {
            (LocationKind::Exact(p.to_string()), None)
        } else if let Some(p) = cfg.path.strip_prefix("^~ ") {
            (LocationKind::PrefixPriority(p.to_string()), None)
        } else {
            (LocationKind::Prefix, None)
        };
        let limit_conn = cfg.limit_conn;
        let access_rules = crate::middleware::access_control::compile_rules(&cfg.access_rules);
        Self { config: cfg, kind, regex, conn_count: Arc::new(AtomicUsize::new(0)), limit_conn, access_rules }
    }
}

/// 在预编译 location 列表中匹配请求路径，返回最优匹配的 LocationConfig
///
/// 正则已在站点启动时预编译，此处直接调用 `is_match`，零堆分配
pub fn match_location<'a>(
    locations: &'a [CompiledLocation],
    path: &str,
) -> Option<&'a CompiledLocation> {
    // 第一轮：精确匹配和前缀优先（用预计算的 kind，零字符串扫描）
    for cl in locations {
        match &cl.kind {
            LocationKind::Exact(p) if p == path => return Some(cl),
            LocationKind::PrefixPriority(p) if path.starts_with(p.as_str()) => return Some(cl),
            _ => {}
        }
    }

    // 第二轮：正则匹配
    for cl in locations {
        if let LocationKind::Regex(re) = &cl.kind {
            if re.is_match(path) {
                return Some(cl);
            }
        }
    }

    // 第三轮：普通前缀匹配，找最长前缀
    let mut best: Option<(&CompiledLocation, usize)> = None;
    for cl in locations {
        if !matches!(cl.kind, LocationKind::Prefix) { continue; }
        let p = &cl.config.path;
        if path.starts_with(p.as_str()) {
            let len = p.len();
            if best.map_or(true, |(_, bl)| len > bl) {
                best = Some((cl, len));
            }
        }
    }

    best.map(|(cl, _)| cl)
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::HandlerType;

    fn cloc(path: &str) -> CompiledLocation {
        CompiledLocation::new(LocationConfig {
            path: path.to_string(),
            handler: HandlerType::Static,
            ..Default::default()
        })
    }

    #[test]
    fn test_exact_match_wins() {
        let locations = vec![cloc("/"), cloc("= /exact")];
        let result = match_location(&locations, "/exact");
        assert_eq!(result.unwrap().config.path, "= /exact");
    }

    #[test]
    fn test_prefix_priority_wins_over_regex() {
        let locations = vec![cloc("~ /static"), cloc("^~ /static/")];
        let result = match_location(&locations, "/static/logo.png");
        assert_eq!(result.unwrap().config.path, "^~ /static/");
    }

    #[test]
    fn test_regex_match() {
        let locations = vec![cloc("/"), cloc("~ \\.php$")];
        let result = match_location(&locations, "/index.php");
        assert_eq!(result.unwrap().config.path, "~ \\.php$");
    }

    #[test]
    fn test_regex_case_insensitive() {
        let locations = vec![cloc("~* \\.PHP$")];
        let result = match_location(&locations, "/Index.PHP");
        assert!(result.is_some());
    }

    #[test]
    fn test_longest_prefix_wins() {
        let locations = vec![cloc("/"), cloc("/api/"), cloc("/api/v1/")];
        let result = match_location(&locations, "/api/v1/users");
        assert_eq!(result.unwrap().config.path, "/api/v1/");
    }

    #[test]
    fn test_no_match_returns_none() {
        let locations = vec![cloc("/api/")];
        let result = match_location(&locations, "/other");
        assert!(result.is_none());
    }

    #[test]
    fn test_root_prefix_match() {
        let locations = vec![cloc("/")];
        let result = match_location(&locations, "/anything");
        assert_eq!(result.unwrap().config.path, "/");
    }
}
