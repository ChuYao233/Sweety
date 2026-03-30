//! 限流中间件
//! 负责：按 IP / 路径 / Header / User-Agent 多维度令牌桶限流

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tracing::debug;

use crate::config::model::{RateLimitDimension, RateLimitRule};

// ─────────────────────────────────────────────
// 令牌桶实现
// ─────────────────────────────────────────────

/// 单个令牌桶状态
#[derive(Debug)]
pub struct TokenBucket {
    /// 当前令牌数（浮点以精确计算补充速率）
    tokens: f64,
    /// 上次更新时间
    last_refill: Instant,
    /// 稳定速率（每秒令牌数）
    rate: f64,
    /// 桶容量（最大突发）
    capacity: f64,
}

impl TokenBucket {
    pub fn new(rate: u64, burst: u64) -> Self {
        let cap = if burst == 0 { rate as f64 } else { burst as f64 };
        Self {
            tokens: cap,
            last_refill: Instant::now(),
            rate: rate as f64,
            capacity: cap,
        }
    }

    /// 尝试消耗 1 个令牌，返回是否允许通过
    pub fn try_acquire(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// 根据经过时间补充令牌
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.capacity);
        self.last_refill = now;
    }
}

// ─────────────────────────────────────────────
// 限流器
// ─────────────────────────────────────────────

/// 单条规则对应的限流器
pub struct RateLimiter {
    /// 限流规则配置
    pub rule: RateLimitRule,
    /// 令牌桶映射表：限流 key → TokenBucket
    buckets: Arc<DashMap<String, TokenBucket>>,
}

impl RateLimiter {
    pub fn new(rule: RateLimitRule) -> Self {
        Self {
            rule,
            buckets: Arc::new(DashMap::new()),
        }
    }

    /// 检查请求是否允许通过
    ///
    /// 参数：
    /// - `client_ip`: 客户端 IP
    /// - `path`: 请求路径
    /// - `headers`: 请求头集合（header_name → value）
    /// - `user_agent`: User-Agent 字符串
    pub fn check(
        &self,
        client_ip: &str,
        path: &str,
        headers: &std::collections::HashMap<String, String>,
        user_agent: &str,
    ) -> RateLimitResult {
        let key = self.build_key(client_ip, path, headers, user_agent);
        let key = match key {
            Some(k) => k,
            None => return RateLimitResult::Allow, // 条件不匹配，放行
        };

        let mut bucket = self.buckets.entry(key).or_insert_with(|| {
            TokenBucket::new(self.rule.rate, self.rule.burst)
        });

        if bucket.try_acquire() {
            RateLimitResult::Allow
        } else {
            RateLimitResult::Deny {
                retry_after_secs: (1.0 / self.rule.rate as f64).ceil() as u64,
            }
        }
    }

    /// 根据限流维度构建 bucket key
    fn build_key(
        &self,
        client_ip: &str,
        path: &str,
        headers: &std::collections::HashMap<String, String>,
        user_agent: &str,
    ) -> Option<String> {
        match self.rule.dimension {
            RateLimitDimension::Ip => Some(client_ip.to_string()),

            RateLimitDimension::Path => {
                if let Some(pattern) = &self.rule.path_pattern {
                    if let Ok(re) = regex::Regex::new(pattern) {
                        if re.is_match(path) {
                            return Some(format!("path:{}", path));
                        }
                    }
                    None // 路径不匹配该规则
                } else {
                    Some(format!("path:{}", path))
                }
            }

            RateLimitDimension::Header => {
                if let Some(header_name) = &self.rule.header_name {
                    let key_lower = header_name.to_lowercase();
                    headers.get(&key_lower).map(|v| format!("header:{}:{}", header_name, v))
                } else {
                    None
                }
            }

            RateLimitDimension::UserAgent => {
                if user_agent.is_empty() {
                    None
                } else {
                    Some(format!("ua:{}", user_agent))
                }
            }
        }
    }

    /// 清理过期的令牌桶（防止内存无限增长）
    ///
    /// 生产版本应定期调用，移除长时间未使用的 bucket
    pub fn cleanup_expired(&self, idle_secs: u64) {
        let threshold = Duration::from_secs(idle_secs);
        self.buckets.retain(|_, bucket| {
            bucket.last_refill.elapsed() < threshold
        });
        debug!("限流桶清理完成，剩余 {} 个活跃桶", self.buckets.len());
    }
}

/// 限流检查结果
#[derive(Debug, PartialEq, Eq)]
pub enum RateLimitResult {
    /// 允许通过
    Allow,
    /// 拒绝，附带建议重试时间（秒）
    Deny { retry_after_secs: u64 },
}

// ─────────────────────────────────────────────
// 站点限流管理器（组合多条规则）
// ─────────────────────────────────────────────

/// 单站点的完整限流管理器（包含多条规则）
#[derive(Default)]
pub struct SiteRateLimiter {
    limiters: Vec<RateLimiter>,
}

impl SiteRateLimiter {
    /// 从规则列表构建
    pub fn from_rules(rules: Vec<RateLimitRule>) -> Self {
        Self {
            limiters: rules.into_iter().map(RateLimiter::new).collect(),
        }
    }

    /// 检查所有规则，任意一条拒绝即返回 Deny
    pub fn check_all(
        &self,
        client_ip: &str,
        path: &str,
        headers: &std::collections::HashMap<String, String>,
        user_agent: &str,
    ) -> RateLimitResult {
        for limiter in &self.limiters {
            if let RateLimitResult::Deny { retry_after_secs } =
                limiter.check(client_ip, path, headers, user_agent)
            {
                return RateLimitResult::Deny { retry_after_secs };
            }
        }
        RateLimitResult::Allow
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{RateLimitDimension, RateLimitRule};

    fn ip_rule(rate: u64, burst: u64) -> RateLimitRule {
        RateLimitRule {
            dimension: RateLimitDimension::Ip,
            rate,
            burst,
            path_pattern: None,
            header_name: None,
        }
    }

    #[test]
    fn test_token_bucket_allows_burst() {
        let mut bucket = TokenBucket::new(10, 20);
        // 突发容量 20，应允许前 20 次
        for _ in 0..20 {
            assert!(bucket.try_acquire());
        }
        // 第 21 次应被拒绝
        assert!(!bucket.try_acquire());
    }

    #[test]
    fn test_ip_rate_limiter_allows_within_burst() {
        let limiter = RateLimiter::new(ip_rule(100, 5));
        let empty_headers = std::collections::HashMap::new();
        for _ in 0..5 {
            assert_eq!(
                limiter.check("1.2.3.4", "/", &empty_headers, ""),
                RateLimitResult::Allow
            );
        }
        assert!(matches!(
            limiter.check("1.2.3.4", "/", &empty_headers, ""),
            RateLimitResult::Deny { .. }
        ));
    }

    #[test]
    fn test_different_ips_isolated() {
        let limiter = RateLimiter::new(ip_rule(1, 1));
        let headers = std::collections::HashMap::new();
        // IP1 消耗完令牌
        assert_eq!(limiter.check("1.1.1.1", "/", &headers, ""), RateLimitResult::Allow);
        assert!(matches!(limiter.check("1.1.1.1", "/", &headers, ""), RateLimitResult::Deny { .. }));
        // IP2 应有自己的桶
        assert_eq!(limiter.check("2.2.2.2", "/", &headers, ""), RateLimitResult::Allow);
    }

    #[test]
    fn test_path_rate_limiter() {
        let rule = RateLimitRule {
            dimension: RateLimitDimension::Path,
            rate: 1,
            burst: 2,
            path_pattern: Some("^/api/".to_string()),
            header_name: None,
        };
        let limiter = RateLimiter::new(rule);
        let headers = std::collections::HashMap::new();
        // /api/ 路径消耗令牌
        assert_eq!(limiter.check("1.1.1.1", "/api/users", &headers, ""), RateLimitResult::Allow);
        assert_eq!(limiter.check("1.1.1.1", "/api/users", &headers, ""), RateLimitResult::Allow);
        // 超出 burst
        assert!(matches!(
            limiter.check("1.1.1.1", "/api/users", &headers, ""),
            RateLimitResult::Deny { .. }
        ));
        // /other/ 不在规则范围内，应放行
        assert_eq!(limiter.check("1.1.1.1", "/other/", &headers, ""), RateLimitResult::Allow);
    }
}
