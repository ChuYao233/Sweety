//! 站点配置 SiteConfig 和 HstsConfig

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

use super::{
    compress::SiteCompressConfig,
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

    /// HSTS 配置（仅对 HTTPS 端口生效，默认开启 max-age=31536000）
    #[serde(default = "default_hsts")]
    pub hsts: Option<HstsConfig>,

    /// 是否作为 fallback 站点
    #[serde(default)]
    pub fallback: bool,

    /// 站点级 gzip 覆盖（旧字段，向后兼容；推荐使用 [sites.compress]）
    #[serde(default)]
    pub gzip: Option<bool>,

    /// 站点级 gzip 压缩等级覆盖（旧字段，向后兼容）
    #[serde(default)]
    pub gzip_comp_level: Option<u32>,

    /// 站点级压缩配置覆盖（gzip / brotli / zstd，优先于旧字段 gzip/gzip_comp_level）
    #[serde(default)]
    pub compress: SiteCompressConfig,

    /// 是否启用 WebSocket 支持
    #[serde(default = "default_true")]
    pub websocket: bool,

    /// 是否强制 HTTP 跳转到 HTTPS（默认 true，对标 Caddy）
    #[serde(default = "default_true")]
    pub force_https: bool,

    /// Real IP 配置（等价 Nginx set_real_ip_from + real_ip_header）
    #[serde(default)]
    pub real_ip: Option<crate::middleware::real_ip::RealIpConfig>,

    /// 是否在该站点的监听端口上启用 PROXY protocol 解析
    /// 启用后，Sweety 会从入站连接的第一个数据包解析 PROXY protocol v1/v2 头，
    /// 提取真实客户端 IP（适用于 CDN/LB → Sweety 场景）
    /// ⚠️ 仅当前置代理确实发送 PROXY protocol 时才启用，否则连接会断开
    #[serde(default)]
    pub proxy_protocol: bool,

    /// 自定义错误页（等价 Nginx error_page 404 /404.html）
    #[serde(default, deserialize_with = "deserialize_u16_map")]
    pub error_pages: HashMap<u16, String>,

    /// 反代响应缓存配置（等价 Nginx proxy_cache）
    #[serde(default)]
    pub proxy_cache: Option<ProxyCacheConfig>,

    // ─── 开箱即用语法糖（Caddy 风格，加载时自动展开） ───────────────────────

    /// 内置应用预设，自动生成对应 location 规则
    ///
    /// 可用值：`wordpress` / `laravel` / `static`
    /// 若已手动配置 `[[sites.locations]]`，preset 不覆盖（手动优先）
    ///
    /// ```toml
    /// preset = "wordpress"
    /// ```
    #[serde(default)]
    pub preset: Option<crate::config::preset::SitePreset>,

    /// PHP FastCGI 快捷字段（Unix socket 路径或 host:port）
    ///
    /// 等同完整 `[sites.fastcgi]` 块，pool_size/timeout 使用默认値。
    /// 若已配置 `[sites.fastcgi]`，此字段被忽略。
    ///
    /// ```toml
    /// php_fastcgi = "/tmp/php-cgi-82.sock"
    /// php_fastcgi = "127.0.0.1:9000"
    /// ```
    #[serde(default)]
    pub php_fastcgi: Option<String>,

    /// ACME 自动 HTTPS 快捷字段
    ///
    /// 配置此字段后，无需再写完整的 `[sites.tls]` 块，
    /// expand 阶段会自动启用 ACME 并使用 Let's Encrypt 申请证书。
    /// 若已配置 `[sites.tls]`，此字段被忽略。
    ///
    /// ```toml
    /// listen_tls  = [443]
    /// acme_email  = "your@example.com"
    /// ```
    #[serde(default)]
    pub acme_email: Option<String>,
}

fn default_hsts_max_age() -> u64 { 31_536_000 }
fn default_hsts() -> Option<HstsConfig> { Some(HstsConfig::default()) }

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
