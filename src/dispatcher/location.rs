//! Location 路径匹配模块
//! 负责：在单个站点内按优先级匹配请求路径到 LocationConfig
//! 优先级规则参照 Nginx：精确 > 前缀优先 > 正则 > 普通前缀

use regex::Regex;

use crate::config::model::LocationConfig;

/// 在 location 列表中匹配请求路径，返回最优匹配
///
/// locations 应已按优先级排序（由 VHostRegistry 构建时完成）
pub fn match_location<'a>(
    locations: &'a [LocationConfig],
    path: &str,
) -> Option<&'a LocationConfig> {
    // 第一轮：精确匹配 (`= /path`) 和 前缀优先 (`^~ /prefix`)
    // 找到即立刻返回，不再继续
    for loc in locations {
        if let Some(stripped) = loc.path.strip_prefix("= ") {
            if stripped == path {
                return Some(loc);
            }
        } else if let Some(stripped) = loc.path.strip_prefix("^~ ") {
            if path.starts_with(stripped) {
                return Some(loc);
            }
        }
    }

    // 第二轮：正则匹配 (`~ pattern` 区分大小写，`~* pattern` 不区分)
    for loc in locations {
        let (pattern, ignore_case) = if let Some(p) = loc.path.strip_prefix("~* ") {
            (p, true)
        } else if let Some(p) = loc.path.strip_prefix("~ ") {
            (p, false)
        } else {
            continue;
        };

        let regex_result = if ignore_case {
            Regex::new(&format!("(?i){}", pattern))
        } else {
            Regex::new(pattern)
        };

        if let Ok(re) = regex_result {
            if re.is_match(path) {
                return Some(loc);
            }
        }
    }

    // 第三轮：普通前缀匹配，找最长前缀
    let mut best: Option<(&LocationConfig, usize)> = None;
    for loc in locations {
        // 跳过已处理的特殊前缀
        if loc.path.starts_with("= ")
            || loc.path.starts_with("^~ ")
            || loc.path.starts_with("~ ")
            || loc.path.starts_with("~* ")
        {
            continue;
        }
        let prefix = &loc.path;
        if path.starts_with(prefix.as_str()) {
            let len = prefix.len();
            if best.map_or(true, |(_, best_len)| len > best_len) {
                best = Some((loc, len));
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

    fn loc(path: &str) -> LocationConfig {
        LocationConfig {
            path: path.to_string(),
            handler: HandlerType::Static,
            root: None,
            upstream: None,
            cache_control: None,
            return_code: None,
            max_connections: None,
        }
    }

    #[test]
    fn test_exact_match_wins() {
        let locations = vec![loc("/"), loc("= /exact")];
        let result = match_location(&locations, "/exact");
        assert_eq!(result.unwrap().path, "= /exact");
    }

    #[test]
    fn test_prefix_priority_wins_over_regex() {
        let locations = vec![loc("~ /static"), loc("^~ /static/")];
        let result = match_location(&locations, "/static/logo.png");
        assert_eq!(result.unwrap().path, "^~ /static/");
    }

    #[test]
    fn test_regex_match() {
        let locations = vec![loc("/"), loc("~ \\.php$")];
        let result = match_location(&locations, "/index.php");
        assert_eq!(result.unwrap().path, "~ \\.php$");
    }

    #[test]
    fn test_regex_case_insensitive() {
        let locations = vec![loc("~* \\.PHP$")];
        let result = match_location(&locations, "/Index.PHP");
        assert!(result.is_some());
    }

    #[test]
    fn test_longest_prefix_wins() {
        let locations = vec![loc("/"), loc("/api/"), loc("/api/v1/")];
        let result = match_location(&locations, "/api/v1/users");
        assert_eq!(result.unwrap().path, "/api/v1/");
    }

    #[test]
    fn test_no_match_returns_none() {
        let locations = vec![loc("/api/")];
        let result = match_location(&locations, "/other");
        assert!(result.is_none());
    }

    #[test]
    fn test_root_prefix_match() {
        let locations = vec![loc("/")];
        let result = match_location(&locations, "/anything");
        assert_eq!(result.unwrap().path, "/");
    }
}
