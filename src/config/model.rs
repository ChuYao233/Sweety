//! 配置结构体定义模块
//!
//! 按职责拆分为子模块：
//! - [`global`]：全局配置（GlobalConfig）
//! - [`site`]：站点配置（SiteConfig、HstsConfig）
//! - [`tls`]：TLS / ACME / HTTP3 配置
//! - [`location`]：Location 路由规则及关联类型
//! - [`upstream`]：上游服务器组及 FastCGI 配置

use serde::{Deserialize, Serialize};

mod global;
mod location;
mod site;
mod tls;
mod upstream;

pub use global::GlobalConfig;
pub use location::{
    CacheRule, HandlerType, HeaderOverride, LocationConfig, ProxyCacheConfig,
    RateLimitConfig, RateLimitDimension, RateLimitRule, RewriteFlag, RewriteRule, SubFilter,
};
pub use site::{HstsConfig, SiteConfig};
pub use tls::{CertKeyPair, DnsProviderConfig, Http3Config, TlsConfig};
pub use upstream::{
    CircuitBreakerConfig, FastCgiCacheConfig, FastCgiConfig, HealthCheckConfig,
    LoadBalanceStrategy, UpstreamAddr, UpstreamConfig, UpstreamNode,
};

// ─────────────────────────────────────────────
// 顶层配置
// ─────────────────────────────────────────────

/// 整体配置，对应配置文件根节点
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AppConfig {
    /// 全局选项
    #[serde(default)]
    pub global: GlobalConfig,

    /// 站点列表
    #[serde(default)]
    pub sites: Vec<SiteConfig>,
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_global_config() {
        let cfg = GlobalConfig::default();
        assert_eq!(cfg.worker_threads, 0);
        assert!(cfg.prometheus_enabled);
        assert_eq!(cfg.prometheus_path, "/metrics");
    }

    #[test]
    fn test_deserialize_minimal_site() {
        let toml_str = r#"
            name = "test"
            server_name = ["localhost"]
        "#;
        let site: SiteConfig = toml::from_str(toml_str).expect("反序列化失败");
        assert_eq!(site.name, "test");
        assert_eq!(site.listen, vec![80]);
        assert_eq!(site.index, vec!["index.html", "index.htm"]);
    }

    #[test]
    fn test_handler_type_serde() {
        let json = r#""reverse_proxy""#;
        let ht: HandlerType = serde_json::from_str(json).unwrap();
        assert_eq!(ht, HandlerType::ReverseProxy);
    }

    #[test]
    fn test_rewrite_flag_default() {
        let rule: RewriteRule = toml::from_str(r#"
            pattern = "^/(.*)$"
            target  = "/index.php?$1"
        "#).unwrap();
        assert_eq!(rule.flag, RewriteFlag::Last);
    }
}
