//! 虚拟主机（VHost）匹配模块
//! 负责：根据请求的 Host header 找到对应的站点配置
//! 支持精确匹配、通配符匹配（*.example.com）

use std::collections::HashMap;
use std::sync::RwLock;

use crate::config::model::{FastCgiConfig, HstsConfig, LocationConfig, RewriteRule, SiteConfig, TlsConfig, UpstreamConfig};

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
    /// FastCGI 配置
    pub fastcgi: Option<FastCgiConfig>,
    /// HSTS 配置
    pub hsts: Option<HstsConfig>,
    /// 是否作为 fallback 站点
    pub fallback: bool,
    /// 是否启用 WebSocket 升级
    pub websocket: bool,
    /// 站点级 gzip 开关覆盖（None = 继承全局）
    pub gzip: Option<bool>,
    /// 站点级 gzip 压缩等级覆盖
    pub gzip_comp_level: Option<u32>,
    /// 是否强制 HTTP 跳转到 HTTPS
    pub force_https: bool,
    /// 站点 TLS 端口列表（用于构造跳转目标 URL）
    pub listen_tls: Vec<u16>,
    /// 自定义错误页（状态码 → 文件路径）
    pub error_pages: std::collections::HashMap<u16, String>,
    /// 反代响应缓存配置
    pub proxy_cache: Option<crate::config::model::ProxyCacheConfig>,
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
            fastcgi: cfg.fastcgi.clone(),
            hsts: cfg.hsts.clone(),
            fallback: cfg.fallback,
            websocket: cfg.websocket,
            gzip: cfg.gzip,
            gzip_comp_level: cfg.gzip_comp_level,
            force_https: cfg.force_https && !cfg.listen_tls.is_empty(),
            listen_tls: cfg.listen_tls.clone(),
            error_pages: cfg.error_pages.clone(),
            proxy_cache: cfg.proxy_cache.clone(),
        }
    }
}

/// 虚拟主机注册表内部数据（由 RwLock 保护）
#[derive(Debug, Default)]
struct VHostInner {
    /// 精确 Host 匹配表
    exact: HashMap<String, SiteInfo>,
    /// 通配符后缀匹配表
    wildcard: HashMap<String, SiteInfo>,
    /// 显式指定的 fallback 站点（fallback = true）
    fallback: Option<SiteInfo>,
}

/// 虚拟主机注册表
///
/// 内部用 RwLock 保护，支持运行时原地增删改站点（热重载不断连）。
#[derive(Debug, Default)]
pub struct VHostRegistry {
    inner: RwLock<VHostInner>,
}

impl VHostRegistry {
    /// 从配置列表构建注册表
    pub fn from_config(sites: &[SiteConfig]) -> Self {
        let registry = Self::default();
        for site_cfg in sites {
            registry.upsert_site(site_cfg);
        }
        registry
    }

    /// 插入或更新单个站点（热重载时只更新变化的站点）
    pub fn upsert_site(&self, site_cfg: &SiteConfig) {
        let site_info = SiteInfo::from_config(site_cfg);
        let mut inner = self.inner.write().unwrap();
        // 先清除该站点所有旧条目（防止 server_name 变化时残留）
        inner.exact.retain(|_, v| v.name != site_cfg.name);
        inner.wildcard.retain(|_, v| v.name != site_cfg.name);
        for server_name in &site_cfg.server_name {
            if server_name.starts_with("*.") {
                let suffix = server_name[2..].to_lowercase();
                inner.wildcard.insert(suffix, site_info.clone());
            } else {
                inner.exact.insert(server_name.to_lowercase(), site_info.clone());
            }
        }
        // fallback = true 的站点单独存储，不参与正常匹配
        if site_cfg.fallback {
            inner.fallback = Some(site_info);
        }
    }

    /// 删除单个站点
    pub fn remove_site(&self, site_name: &str) {
        let mut inner = self.inner.write().unwrap();
        inner.exact.retain(|_, v| v.name != site_name);
        inner.wildcard.retain(|_, v| v.name != site_name);
        if inner.fallback.as_ref().map(|d| d.name == site_name).unwrap_or(false) {
            inner.fallback = None;
        }
    }

    /// 根据 Host 字符串查找站点（HTTP：不匹配时返回显式指定的 fallback 站点）
    pub fn lookup(&self, host: &str) -> Option<SiteInfo> {
        let host = strip_port(host);
        let host_lower = host.to_lowercase();
        let inner = self.inner.read().unwrap();
        if let Some(site) = inner.exact.get(&host_lower) {
            return Some(site.clone());
        }
        if let Some(dot_pos) = host_lower.find('.') {
            let suffix = &host_lower[dot_pos + 1..];
            if let Some(site) = inner.wildcard.get(suffix) {
                return Some(site.clone());
            }
        }
        // 只返回显式标记为 fallback 的站点
        inner.fallback.clone()
    }

    /// 严格匹配：HTTPS 请求防跨站用
    ///
    /// - 精确/通配符匹配：返回匹配站点
    /// - 不匹配：返回显式标记 fallback=true 的站点（存在的话）
    /// - 无 fallback 站点时：返回 None（调用方返回 421）
    pub fn lookup_strict(&self, host: &str) -> Option<SiteInfo> {
        let host = strip_port(host);
        let host_lower = host.to_lowercase();
        let inner = self.inner.read().unwrap();
        if let Some(site) = inner.exact.get(&host_lower) {
            return Some(site.clone());
        }
        if let Some(dot_pos) = host_lower.find('.') {
            let suffix = &host_lower[dot_pos + 1..];
            if let Some(site) = inner.wildcard.get(suffix) {
                return Some(site.clone());
            }
        }
        // 返回 fallback 站点（若配置了）
        inner.fallback.clone()
    }

    /// 返回注册的站点总数
    pub fn site_count(&self) -> usize {
        let inner = self.inner.read().unwrap();
        let names: std::collections::HashSet<&str> =
            inner.exact.values().map(|s| s.name.as_str()).collect();
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
                strip_cookie_secure: false,
                proxy_cookie_domain: None,
                proxy_redirect_from: None,
                proxy_redirect_to: None,
                proxy_set_headers: vec![],
                add_headers: vec![],
                cache_rules: vec![],
                return_url: None,
                try_files: vec![],
                sub_filter: vec![],
            }],
            rewrites: vec![],
            rate_limit: None,
            hsts: None,
            fallback: false,
            gzip: None,
            gzip_comp_level: None,
            websocket: true,
            force_https: false,
            error_pages: std::collections::HashMap::new(),
            proxy_cache: None,
        }
    }

    fn make_fallback_site(name: &str) -> SiteConfig {
        let mut s = make_site(name, &["fallback.internal"]);
        s.fallback = true;
        s
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
        // 无 fallback 站点时不匹配返回 None
        assert!(reg.lookup("example.com").is_none());
    }

    #[test]
    fn test_strip_port() {
        assert_eq!(strip_port("example.com:8080"), "example.com");
        assert_eq!(strip_port("example.com"), "example.com");
        assert_eq!(strip_port("[::1]:8080"), "[::1]");
    }

    #[test]
    fn test_fallback_site() {
        let sites = vec![
            make_site("first", &["first.com"]),
            make_fallback_site("default"),
        ];
        let reg = VHostRegistry::from_config(&sites);
        // 显式 fallback 站点才会被返回
        assert_eq!(reg.lookup("unknown.com").map(|s| s.name.as_str()), Some("default"));
        // 无 fallback 站点时不匹配返回 None
        let sites2 = vec![make_site("only", &["only.com"])];
        let reg2 = VHostRegistry::from_config(&sites2);
        assert!(reg2.lookup("unknown.com").is_none());
    }

    #[test]
    fn test_location_priority() {
        assert!(location_priority("= /exact") < location_priority("^~ /prefix"));
        assert!(location_priority("^~ /prefix") < location_priority("~ .php$"));
        assert!(location_priority("~ .php$") < location_priority("/"));
    }
}
