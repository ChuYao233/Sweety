//! proxy_next_upstream 重试条件控制
//!
//! 等价 Nginx `proxy_next_upstream`，细粒度控制哪些错误触发上游重试。
//! 使用 bitflags 实现，热路径零分配、O(1) 判断。

use serde::{Deserialize, Serialize};

/// 重试条件位标志（每个条件占 1 bit，可自由组合）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NextUpstreamFlags(u16);

impl NextUpstreamFlags {
    /// 连接错误（refused / reset / IO error）
    pub const ERROR: u16       = 0b_0000_0001;
    /// 连接/读取/写入超时
    pub const TIMEOUT: u16     = 0b_0000_0010;
    /// 上游返回 HTTP 502
    pub const HTTP_502: u16    = 0b_0000_0100;
    /// 上游返回 HTTP 503
    pub const HTTP_503: u16    = 0b_0000_1000;
    /// 上游返回 HTTP 504
    pub const HTTP_504: u16    = 0b_0001_0000;
    /// 上游返回 HTTP 429
    pub const HTTP_429: u16    = 0b_0010_0000;
    /// 允许对非幂等方法（POST/PATCH/DELETE）重试
    pub const NON_IDEMPOTENT: u16 = 0b_0100_0000;
    /// 响应头无效（解析失败）
    pub const INVALID_HEADER: u16 = 0b_1000_0000;

    /// 默认：error + timeout（与 Nginx 默认一致）
    pub const DEFAULT: Self = Self(Self::ERROR | Self::TIMEOUT);

    /// 关闭所有重试
    pub const OFF: Self = Self(0);

    #[inline(always)]
    pub const fn contains(self, flag: u16) -> bool {
        (self.0 & flag) != 0
    }

    #[inline(always)]
    pub const fn is_off(self) -> bool {
        self.0 == 0
    }

    /// 判断给定的 HTTP 状态码是否匹配重试条件
    #[inline]
    pub fn matches_status(self, status: u16) -> bool {
        match status {
            502 => self.contains(Self::HTTP_502),
            503 => self.contains(Self::HTTP_503),
            504 => self.contains(Self::HTTP_504),
            429 => self.contains(Self::HTTP_429),
            _   => false,
        }
    }

    /// 判断是否允许对非幂等方法重试
    #[inline(always)]
    pub const fn allows_non_idempotent(self) -> bool {
        self.contains(Self::NON_IDEMPOTENT)
    }
}

/// 配置层的重试条件列表（serde 友好）
///
/// 配置示例：
/// ```toml
/// proxy_next_upstream = ["error", "timeout", "http_502"]
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NextUpstreamCondition {
    /// 连接错误
    Error,
    /// 超时
    Timeout,
    /// HTTP 502
    Http502,
    /// HTTP 503
    Http503,
    /// HTTP 504
    Http504,
    /// HTTP 429
    Http429,
    /// 允许非幂等方法重试
    NonIdempotent,
    /// 响应头无效
    InvalidHeader,
    /// 关闭重试
    Off,
}

/// 从配置 Vec 编译为 bitflags（启动时一次性，运行时零开销）
pub fn compile_flags(conditions: &[NextUpstreamCondition]) -> NextUpstreamFlags {
    if conditions.is_empty() {
        return NextUpstreamFlags::DEFAULT;
    }
    // "off" 优先级最高
    if conditions.contains(&NextUpstreamCondition::Off) {
        return NextUpstreamFlags::OFF;
    }
    let mut bits: u16 = 0;
    for c in conditions {
        bits |= match c {
            NextUpstreamCondition::Error         => NextUpstreamFlags::ERROR,
            NextUpstreamCondition::Timeout       => NextUpstreamFlags::TIMEOUT,
            NextUpstreamCondition::Http502       => NextUpstreamFlags::HTTP_502,
            NextUpstreamCondition::Http503       => NextUpstreamFlags::HTTP_503,
            NextUpstreamCondition::Http504       => NextUpstreamFlags::HTTP_504,
            NextUpstreamCondition::Http429       => NextUpstreamFlags::HTTP_429,
            NextUpstreamCondition::NonIdempotent => NextUpstreamFlags::NON_IDEMPOTENT,
            NextUpstreamCondition::InvalidHeader => NextUpstreamFlags::INVALID_HEADER,
            NextUpstreamCondition::Off           => 0, // 上面已处理
        };
    }
    NextUpstreamFlags(bits)
}

/// 判断是否为幂等方法（GET/HEAD/PUT/DELETE/OPTIONS/TRACE）
#[inline]
pub fn is_idempotent(method: &str) -> bool {
    matches!(method, "GET" | "HEAD" | "PUT" | "DELETE" | "OPTIONS" | "TRACE")
}

/// 根据错误类型和 flags 判断是否应触发上游重试
///
/// 从 `anyhow::Error` 中提取 `ProxyError` 变体，映射到对应的 flag bit。
#[inline]
pub fn should_retry_error(err: &anyhow::Error, flags: NextUpstreamFlags) -> bool {
    use super::error::ProxyError;
    if flags.is_off() { return false; }

    if let Some(pe) = err.downcast_ref::<ProxyError>() {
        match pe {
            // 连接类错误 → ERROR flag
            ProxyError::ConnRefused { .. }
            | ProxyError::ConnReset { .. }
            | ProxyError::TlsHandshake { .. }
            | ProxyError::Io { .. } => flags.contains(NextUpstreamFlags::ERROR),

            // 超时类错误 → TIMEOUT flag
            ProxyError::ConnTimeout { .. }
            | ProxyError::ReadTimeout { .. }
            | ProxyError::WriteTimeout { .. }
            | ProxyError::TtfbTimeout { .. } => flags.contains(NextUpstreamFlags::TIMEOUT),

            // 响应解析错误 → INVALID_HEADER flag
            ProxyError::InvalidResponse { .. } => flags.contains(NextUpstreamFlags::INVALID_HEADER),

            // 5xx 在 Ok 分支通过 matches_status 处理，这里不重复
            ProxyError::BadGateway { .. } => false,

            // 无可用节点不应重试
            ProxyError::NoAvailableUpstream { .. } => false,
        }
    } else {
        // 未知错误类型，按 ERROR 条件判断
        flags.contains(NextUpstreamFlags::ERROR)
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_flags() {
        let f = NextUpstreamFlags::DEFAULT;
        assert!(f.contains(NextUpstreamFlags::ERROR));
        assert!(f.contains(NextUpstreamFlags::TIMEOUT));
        assert!(!f.contains(NextUpstreamFlags::HTTP_502));
        assert!(!f.allows_non_idempotent());
    }

    #[test]
    fn test_compile_empty_is_default() {
        assert_eq!(compile_flags(&[]), NextUpstreamFlags::DEFAULT);
    }

    #[test]
    fn test_compile_off() {
        let f = compile_flags(&[NextUpstreamCondition::Error, NextUpstreamCondition::Off]);
        assert!(f.is_off());
    }

    #[test]
    fn test_compile_custom() {
        let f = compile_flags(&[
            NextUpstreamCondition::Error,
            NextUpstreamCondition::Http502,
            NextUpstreamCondition::Http503,
        ]);
        assert!(f.contains(NextUpstreamFlags::ERROR));
        assert!(f.contains(NextUpstreamFlags::HTTP_502));
        assert!(f.contains(NextUpstreamFlags::HTTP_503));
        assert!(!f.contains(NextUpstreamFlags::TIMEOUT));
    }

    #[test]
    fn test_matches_status() {
        let f = compile_flags(&[NextUpstreamCondition::Http502, NextUpstreamCondition::Http504]);
        assert!(f.matches_status(502));
        assert!(!f.matches_status(503));
        assert!(f.matches_status(504));
        assert!(!f.matches_status(200));
    }

    #[test]
    fn test_idempotent() {
        assert!(is_idempotent("GET"));
        assert!(is_idempotent("HEAD"));
        assert!(is_idempotent("PUT"));
        assert!(!is_idempotent("POST"));
        assert!(!is_idempotent("PATCH"));
    }
}
