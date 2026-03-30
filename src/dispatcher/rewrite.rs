//! Rewrite / 伪静态规则引擎
//! 负责：按顺序应用 Rewrite 规则，支持正则捕获组替换、条件判断、标志位处理

use regex::Regex;

use crate::config::model::{RewriteFlag, RewriteRule};

/// 对请求路径应用 Rewrite 规则列表
///
/// 返回值：
/// - `Some(new_path)` 表示路径被重写
/// - `None` 表示没有规则匹配，保持原路径
pub fn apply_rewrites(rules: &[RewriteRule], path: &str) -> Option<String> {
    let mut current = path.to_string();
    let mut changed = false;

    for rule in rules {
        // 检查触发条件（如 !-f 文件不存在）
        if let Some(cond) = &rule.condition {
            if !evaluate_condition(cond, &current) {
                continue;
            }
        }

        // 编译正则（生产版本应缓存编译结果，此处简单实现）
        let re = match Regex::new(&rule.pattern) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("Rewrite 规则正则编译失败 '{}': {}", rule.pattern, e);
                continue;
            }
        };

        if !re.is_match(&current) {
            continue;
        }

        // 执行捕获组替换（$1 → 第1组，$2 → 第2组 …）
        let new_path = regex_replace(&re, &current, &rule.target);
        current = new_path;
        changed = true;

        match rule.flag {
            RewriteFlag::Last | RewriteFlag::Break => {
                // last/break 都停止继续处理后续 rewrite
                break;
            }
            RewriteFlag::Redirect | RewriteFlag::Permanent => {
                // 重定向标志：停止处理，上层需要发送重定向响应
                // 此处在路径前加标记前缀供 dispatcher 识别
                // 格式：`REDIRECT:302:<new_path>` 或 `REDIRECT:301:<new_path>`
                let code = if rule.flag == RewriteFlag::Permanent {
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
/// 目前支持：
/// - `!-f`  文件不存在（当前实现始终返回 true，完整版需要检查文件系统）
/// - `!-d`  目录不存在
/// - `-f`   文件存在
/// - `-d`   目录存在
fn evaluate_condition(condition: &str, _path: &str) -> bool {
    match condition.trim() {
        "!-f" | "!-d" => {
            // TODO（v0.2）：根据站点 root + path 检查文件/目录是否存在
            // 当前简化实现：条件始终满足（触发 rewrite）
            true
        }
        "-f" | "-d" => {
            // TODO（v0.2）：检查文件/目录存在
            false
        }
        _ => {
            tracing::warn!("不支持的 Rewrite 条件: '{}'，跳过", condition);
            false
        }
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{RewriteFlag, RewriteRule};

    fn rule(pattern: &str, target: &str, flag: RewriteFlag) -> RewriteRule {
        RewriteRule {
            pattern: pattern.to_string(),
            target: target.to_string(),
            flag,
            condition: None,
        }
    }

    fn rule_with_cond(
        pattern: &str,
        target: &str,
        flag: RewriteFlag,
        cond: &str,
    ) -> RewriteRule {
        RewriteRule {
            pattern: pattern.to_string(),
            target: target.to_string(),
            flag,
            condition: Some(cond.to_string()),
        }
    }

    #[test]
    fn test_basic_rewrite() {
        let rules = vec![rule("^/old/(.*)$", "/new/$1", RewriteFlag::Last)];
        let result = apply_rewrites(&rules, "/old/page");
        assert_eq!(result, Some("/new/page".to_string()));
    }

    #[test]
    fn test_no_match_returns_none() {
        let rules = vec![rule("^/api/", "/backend/", RewriteFlag::Last)];
        let result = apply_rewrites(&rules, "/other/path");
        assert!(result.is_none());
    }

    #[test]
    fn test_wordpress_style_rewrite() {
        let rules = vec![rule("^/(.+)$", "/index.php?$1", RewriteFlag::Last)];
        let result = apply_rewrites(&rules, "/hello-world");
        assert_eq!(result, Some("/index.php?hello-world".to_string()));
    }

    #[test]
    fn test_redirect_flag() {
        let rules = vec![rule("^/old$", "/new", RewriteFlag::Redirect)];
        let result = apply_rewrites(&rules, "/old");
        assert_eq!(result, Some("REDIRECT:302:/new".to_string()));
    }

    #[test]
    fn test_permanent_redirect_flag() {
        let rules = vec![rule("^/old$", "/new", RewriteFlag::Permanent)];
        let result = apply_rewrites(&rules, "/old");
        assert_eq!(result, Some("REDIRECT:301:/new".to_string()));
    }

    #[test]
    fn test_break_stops_chain() {
        let rules = vec![
            rule("^/(.*)$", "/first/$1", RewriteFlag::Break),
            rule("^/first/(.*)$", "/second/$1", RewriteFlag::Last),
        ];
        let result = apply_rewrites(&rules, "/page");
        // break 后不应继续处理第二条规则
        assert_eq!(result, Some("/first/page".to_string()));
    }

    #[test]
    fn test_condition_not_file() {
        // !-f 条件：当前简化实现始终为 true，规则应触发
        let rules = vec![rule_with_cond(
            "^/(.*)$",
            "/index.php?$1",
            RewriteFlag::Last,
            "!-f",
        )];
        let result = apply_rewrites(&rules, "/hello");
        assert_eq!(result, Some("/index.php?hello".to_string()));
    }
}
