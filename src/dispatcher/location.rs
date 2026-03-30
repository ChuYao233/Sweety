//! Location 路径匹配模块
//! 负责：在单个站点内按优先级匹配请求路径到 LocationConfig
//! 优先级规则参照 Nginx：精确 > 前缀优先 > 正则 > 普通前缀
//!
//! 性能关键点：正则对象在站点初始化时预编译，请求时直接匹配，避免每请求 Regex::new()

use regex::Regex;

use crate::config::model::LocationConfig;

/// 预编译后的 Location（所有正则在站点启动时一次编译）
pub struct CompiledLocation {
    /// 原始配置
    pub config: LocationConfig,
    /// 预编译的正则（只有正则类型的 location 才有）
    pub regex: Option<Regex>,
}

impl Clone for CompiledLocation {
    fn clone(&self) -> Self {
        // Regex 支持 Clone
        Self {
            config: self.config.clone(),
            regex: self.regex.clone(),
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
    /// 从 LocationConfig 构建，预编译正则
    pub fn new(cfg: LocationConfig) -> Self {
        let regex = if let Some(p) = cfg.path.strip_prefix("~* ") {
            Regex::new(&format!("(?i){}", p)).ok()
        } else if let Some(p) = cfg.path.strip_prefix("~ ") {
            Regex::new(p).ok()
        } else {
            None
        };
        Self { config: cfg, regex }
    }
}

/// 在预编译 location 列表中匹配请求路径，返回最优匹配的 LocationConfig
///
/// 正则已在站点启动时预编译，此处直接调用 `is_match`，零堆分配
pub fn match_location<'a>(
    locations: &'a [CompiledLocation],
    path: &str,
) -> Option<&'a LocationConfig> {
    // 第一轮：精确匹配 (`= /path`) 和 前缀优先 (`^~ /prefix`)
    for cl in locations {
        if let Some(stripped) = cl.config.path.strip_prefix("= ") {
            if stripped == path {
                return Some(&cl.config);
            }
        } else if let Some(stripped) = cl.config.path.strip_prefix("^~ ") {
            if path.starts_with(stripped) {
                return Some(&cl.config);
            }
        }
    }

    // 第二轮：正则匹配（直接用预编译的 Regex）
    for cl in locations {
        if let Some(re) = &cl.regex {
            if re.is_match(path) {
                return Some(&cl.config);
            }
        }
    }

    // 第三轮：普通前缀匹配，找最长前缀
    let mut best: Option<(&LocationConfig, usize)> = None;
    for cl in locations {
        let p = &cl.config.path;
        if p.starts_with("= ") || p.starts_with("^~ ") || cl.regex.is_some() {
            continue;
        }
        if path.starts_with(p.as_str()) {
            let len = p.len();
            if best.map_or(true, |(_, bl)| len > bl) {
                best = Some((&cl.config, len));
            }
        }
    }

    best.map(|(loc, _)| loc)
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
            root: None,
            upstream: None,
            cache_control: None,
            return_code: None,
            max_connections: None,
        })
    }

    #[test]
    fn test_exact_match_wins() {
        let locations = vec![cloc("/"), cloc("= /exact")];
        let result = match_location(&locations, "/exact");
        assert_eq!(result.unwrap().path, "= /exact");
    }

    #[test]
    fn test_prefix_priority_wins_over_regex() {
        let locations = vec![cloc("~ /static"), cloc("^~ /static/")];
        let result = match_location(&locations, "/static/logo.png");
        assert_eq!(result.unwrap().path, "^~ /static/");
    }

    #[test]
    fn test_regex_match() {
        let locations = vec![cloc("/"), cloc("~ \\.php$")];
        let result = match_location(&locations, "/index.php");
        assert_eq!(result.unwrap().path, "~ \\.php$");
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
        assert_eq!(result.unwrap().path, "/api/v1/");
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
        assert_eq!(result.unwrap().path, "/");
    }
}
