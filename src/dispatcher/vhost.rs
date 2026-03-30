//! 虚拟主机（VHost）匹配模块
//! 负责：根据请求的 Host header 找到对应的站点配置
//! 支持精确匹配、通配符匹配（*.example.com）

use std::collections::HashMap;

use crate::config::model::{LocationConfig, RewriteRule, SiteConfig, TlsConfig, UpstreamConfig};

/// 运行时站点信息（从 SiteConfig 提取，去掉不需要运行时使用的字段）
#[derive(Debug, Clone)]
pub struct SiteInfo {
    /// 站点名称
    pub name: String,
    /// 站点根目录
    pub root: Option<std::path::PathBuf>,
    /// 默认文档列表
    pub index: Vec<String>,
    /// Location 列表（已按优先级排序）
    pub locations: Vec<LocationConfig>,
    /// Rewrite 规则列表
    pub rewrites: Vec<RewriteRule>,
    /// 上游服务器组列表
    pub upstreams: Vec<UpstreamConfig>,
    /// TLS 配置
    pub tls: Option<TlsConfig>,
}

impl SiteInfo {
    /// 从 SiteConfig 转换为运行时 SiteInfo
    pub fn from_config(cfg: &SiteConfig) -> Self {
        let mut locations = cfg.locations.clone();
        // 按匹配优先级排序（精确 > 前缀优先 > 正则 > 普通前缀）
        locations.sort_by_key(|loc| location_priority(&loc.path));

        Self {
            name: cfg.name.clone(),
            root: cfg.root.clone(),
            index: cfg.index.clone(),
            locations,
            rewrites: cfg.rewrites.clone(),
            upstreams: cfg.upstreams.clone(),
            tls: cfg.tls.clone(),
        }
    }
}

/// 虚拟主机注册表
///
/// 维护两张表：
/// - 精确匹配表：`example.com` → SiteInfo
/// - 通配符表：`*.example.com` → SiteInfo（按通配符后缀存储）
#[derive(Debug, Default)]
pub struct VHostRegistry {
    /// 精确 Host 匹配表
    exact: HashMap<String, SiteInfo>,
    /// 通配符后缀匹配表（存储去掉 `*.` 的部分，如 `example.com`）
    wildcard: HashMap<String, SiteInfo>,
    /// 默认站点（当没有任何 Host 匹配时使用第一个站点）
    default: Option<SiteInfo>,
}

impl VHostRegistry {
    /// 从配置列表构建注册表
    pub fn from_config(sites: &[SiteConfig]) -> Self {
        let mut registry = Self::default();

        for site_cfg in sites {
            let site_info = SiteInfo::from_config(site_cfg);

            // 注册第一个站点为默认站点
            if registry.default.is_none() {
                registry.default = Some(site_info.clone());
            }

            for server_name in &site_cfg.server_name {
                if server_name.starts_with("*.") {
                    // 通配符：去掉 `*.` 存储后缀
                    let suffix = server_name[2..].to_lowercase();
                    registry.wildcard.insert(suffix, site_info.clone());
                } else {
                    // 精确匹配
                    registry.exact.insert(server_name.to_lowercase(), site_info.clone());
                }
            }
        }

        registry
    }

    /// 根据 Host 字符串查找站点（包含端口时自动去掉端口部分）
    pub fn lookup(&self, host: &str) -> Option<&SiteInfo> {
        // 去掉端口部分（host:port → host）
        let host = strip_port(host);
        let host_lower = host.to_lowercase();

        // 1. 精确匹配
        if let Some(site) = self.exact.get(&host_lower) {
            return Some(site);
        }

        // 2. 通配符匹配（找第一个匹配的后缀）
        if let Some(dot_pos) = host_lower.find('.') {
            let suffix = &host_lower[dot_pos + 1..];
            if let Some(site) = self.wildcard.get(suffix) {
                return Some(site);
            }
        }

        // 3. 返回默认站点
        self.default.as_ref()
    }

    /// 返回注册的站点总数
    pub fn site_count(&self) -> usize {
        // 精确表中的 name 去重
        let names: std::collections::HashSet<&str> =
            self.exact.values().map(|s| s.name.as_str()).collect();
        names.len()
    }
}

/// 去掉 Host 中的端口部分（处理 IPv6 `[::1]:8080` 格式）
fn strip_port(host: &str) -> &str {
    if host.starts_with('[') {
        // IPv6 格式
        if let Some(end) = host.find(']') {
            return &host[..=end];
        }
    }
    // 普通 host:port 或纯 host
    if let Some(pos) = host.rfind(':') {
        &host[..pos]
    } else {
        host
    }
}

/// 计算 location 路径字符串的匹配优先级（数值越小优先级越高）
///
/// Nginx 匹配优先级：
/// 1 = 精确匹配 (`= /path`)
/// 2 = 前缀优先 (`^~ /prefix`)
/// 3 = 正则匹配 (`~ pattern` 或 `~* pattern`)
/// 4 = 普通前缀 (`/prefix`)
pub(crate) fn location_priority(path: &str) -> u8 {
    if path.starts_with("= ") {
        1
    } else if path.starts_with("^~ ") {
        2
    } else if path.starts_with("~ ") || path.starts_with("~* ") {
        3
    } else {
        4
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{HandlerType, LocationConfig, SiteConfig};

    fn make_site(name: &str, server_names: &[&str]) -> SiteConfig {
        SiteConfig {
            name: name.to_string(),
            server_name: server_names.iter().map(|s| s.to_string()).collect(),
            listen: vec![80],
            listen_tls: vec![],
            root: None,
            index: vec!["index.html".into()],
            access_log: None,
            error_log: None,
            tls: None,
            fastcgi: None,
            upstreams: vec![],
            locations: vec![LocationConfig {
                path: "/".into(),
                handler: HandlerType::Static,
                root: None,
                upstream: None,
                cache_control: None,
                return_code: None,
                max_connections: None,
            }],
            rewrites: vec![],
            rate_limit: None,
        }
    }

    #[test]
    fn test_exact_match() {
        let sites = vec![make_site("demo", &["example.com"])];
        let reg = VHostRegistry::from_config(&sites);
        assert_eq!(reg.lookup("example.com").map(|s| s.name.as_str()), Some("demo"));
    }

    #[test]
    fn test_wildcard_match() {
        let sites = vec![make_site("demo", &["*.example.com"])];
        let reg = VHostRegistry::from_config(&sites);
        assert_eq!(reg.lookup("sub.example.com").map(|s| s.name.as_str()), Some("demo"));
        assert!(reg.lookup("example.com").is_some()); // 回退到默认站点
    }

    #[test]
    fn test_strip_port() {
        assert_eq!(strip_port("example.com:8080"), "example.com");
        assert_eq!(strip_port("example.com"), "example.com");
        assert_eq!(strip_port("[::1]:8080"), "[::1]");
    }

    #[test]
    fn test_default_site_fallback() {
        let sites = vec![make_site("first", &["first.com"])];
        let reg = VHostRegistry::from_config(&sites);
        // 未配置的 host 回退到第一个站点
        assert_eq!(reg.lookup("unknown.com").map(|s| s.name.as_str()), Some("first"));
    }

    #[test]
    fn test_location_priority() {
        assert!(location_priority("= /exact") < location_priority("^~ /prefix"));
        assert!(location_priority("^~ /prefix") < location_priority("~ .php$"));
        assert!(location_priority("~ .php$") < location_priority("/"));
    }
}
