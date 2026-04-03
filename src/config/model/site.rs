//! 站点配置 SiteConfig 和 HstsConfig

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

use super::{
    FastCgiConfig, LocationConfig, ProxyCacheConfig,
    RateLimitConfig, RewriteRule, TlsConfig, UpstreamConfig,
};

/// 将字符串 key 的 HashMap 反序列化为 u16 key（TOML 规范要求 table key 为字符串）
fn deserialize_u16_map<'de, D>(de: D) -> Result<HashMap<u16, String>, D::Error>
where D: Deserializer<'de>
{
    let raw = HashMap::<String, String>::deserialize(de)?;
    raw.into_iter()
        .map(|(k, v)| {
            k.parse::<u16>()
                .map(|n| (n, v))
                .map_err(|_| serde::de::Error::custom(format!("invalid status code key: '{}'", k)))
        })
        .collect()
}

fn default_http_ports() -> Vec<u16> { vec![80] }
fn default_index() -> Vec<String> { vec!["index.html".into(), "index.htm".into()] }
fn default_true() -> bool { true }

/// 单个站点配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SiteConfig {
    /// 站点唯一标识（用于日志、API）
    pub name: String,

    /// 匹配的域名列表，支持通配符 *.example.com
    pub server_name: Vec<String>,

    /// HTTP 监听端口列表
    #[serde(default = "default_http_ports")]
    pub listen: Vec<u16>,

    /// HTTPS 监听端口列表
    #[serde(default)]
    pub listen_tls: Vec<u16>,

    /// 站点根目录（静态文件基准路径）
    #[serde(default)]
    pub root: Option<PathBuf>,

    /// 默认文档列表
    #[serde(default = "default_index")]
    pub index: Vec<String>,

    /// 访问日志路径
    #[serde(default)]
    pub access_log: Option<PathBuf>,

    /// 访问日志格式
    #[serde(default)]
    pub access_log_format: Option<String>,

    /// 错误日志路径（空则使用全局）
    #[serde(default)]
    pub error_log: Option<PathBuf>,

    /// TLS 配置
    #[serde(default)]
    pub tls: Option<TlsConfig>,

    /// FastCGI / PHP 配置
    #[serde(default)]
    pub fastcgi: Option<FastCgiConfig>,

    /// 上游服务器组列表（反向代理）
    #[serde(default)]
    pub upstreams: Vec<UpstreamConfig>,

    /// Location 路由规则列表
    #[serde(default)]
    pub locations: Vec<LocationConfig>,

    /// Rewrite 规则列表
    #[serde(default)]
    pub rewrites: Vec<RewriteRule>,

    /// 限流配置
    #[serde(default)]
    pub rate_limit: Option<RateLimitConfig>,

    /// HSTS 配置（仅对 HTTPS 端口生效）
    #[serde(default)]
    pub hsts: Option<HstsConfig>,

    /// 是否作为 fallback 站点
    #[serde(default)]
    pub fallback: bool,

    /// 站点级 gzip 覆盖（不设则继承全局 global.gzip）
    #[serde(default)]
    pub gzip: Option<bool>,

    /// 站点级 gzip 压缩等级覆盖
    #[serde(default)]
    pub gzip_comp_level: Option<u32>,

    /// 是否启用 WebSocket 支持
    #[serde(default = "default_true")]
    pub websocket: bool,

    /// 是否强制 HTTP 跳转到 HTTPS
    #[serde(default)]
    pub force_https: bool,

    /// 自定义错误页（等价 Nginx error_page 404 /404.html）
    #[serde(default, deserialize_with = "deserialize_u16_map")]
    pub error_pages: HashMap<u16, String>,

    /// 反代响应缓存配置（等价 Nginx proxy_cache）
    #[serde(default)]
    pub proxy_cache: Option<ProxyCacheConfig>,
}

fn default_hsts_max_age() -> u64 { 31_536_000 }

/// HSTS（HTTP Strict Transport Security）配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HstsConfig {
    /// HSTS max-age（秒），0 = 禁用
    #[serde(default = "default_hsts_max_age")]
    pub max_age: u64,

    /// 是否包含 includeSubDomains 指令
    #[serde(default)]
    pub include_sub_domains: bool,

    /// 是否包含 preload 指令
    #[serde(default)]
    pub preload: bool,
}

impl Default for HstsConfig {
    fn default() -> Self {
        Self { max_age: 31_536_000, include_sub_domains: false, preload: false }
    }
}
