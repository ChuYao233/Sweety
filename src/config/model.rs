//! 配置结构体定义模块
//! 所有配置项对应的 Rust 数据结构，通过 serde 实现多格式反序列化

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

/// 全局配置项
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GlobalConfig {
    /// Worker 线程数，0 = CPU 核心数（等价 Nginx worker_processes auto）
    #[serde(default)]
    pub worker_threads: usize,

    /// 每个 worker 最大并发连接数（等价 Nginx worker_connections）
    #[serde(default = "default_worker_connections")]
    pub worker_connections: usize,

    /// Keep-Alive 超时（秒），0 = 禁用（等价 Nginx keepalive_timeout）
    #[serde(default = "default_keepalive_timeout")]
    pub keepalive_timeout: u64,

    /// 客户端最大请求体大小（MB，等价 Nginx client_max_body_size）
    #[serde(default = "default_client_max_body_size")]
    pub client_max_body_size: usize,

    /// 客户端请求头缓冲区大小（KB，等价 Nginx client_header_buffer_size）
    #[serde(default = "default_client_header_buffer_size")]
    pub client_header_buffer_size: usize,

    /// 客户端请求体缓冲区大小（KB，等价 Nginx client_body_buffer_size）
    #[serde(default = "default_client_body_buffer_size")]
    pub client_body_buffer_size: usize,

    /// 是否全局开启 gzip 压缩
    #[serde(default)]
    pub gzip: bool,

    /// gzip 压缩最小文件大小（KB），小于此值不压缩（等价 Nginx gzip_min_length）
    #[serde(default = "default_gzip_min_length")]
    pub gzip_min_length: usize,

    /// gzip 压缩等级 1-9（等价 Nginx gzip_comp_level）
    #[serde(default = "default_gzip_comp_level")]
    pub gzip_comp_level: u32,

    /// 管理 API 监听地址，空字符串表示禁用
    #[serde(default)]
    pub admin_listen: String,

    /// 管理 API Bearer Token
    #[serde(default)]
    pub admin_token: String,

    /// 全局默认错误日志路径
    #[serde(default)]
    pub error_log: Option<PathBuf>,

    /// 是否启用 Prometheus 指标接口
    #[serde(default = "default_true")]
    pub prometheus_enabled: bool,

    /// Prometheus 指标路径（挂载在 admin_listen 上）
    #[serde(default = "default_prometheus_path")]
    pub prometheus_path: String,

    /// 日志级别（error / warn / info / debug / trace）
    /// 也可通过环境变量 RUST_LOG 覆盖（环境变量优先级更高）
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            worker_threads: 0,
            worker_connections: default_worker_connections(),
            keepalive_timeout: default_keepalive_timeout(),
            client_max_body_size: default_client_max_body_size(),
            client_header_buffer_size: default_client_header_buffer_size(),
            client_body_buffer_size: default_client_body_buffer_size(),
            gzip: false,
            gzip_min_length: default_gzip_min_length(),
            gzip_comp_level: default_gzip_comp_level(),
            admin_listen: String::new(),
            admin_token: String::new(),
            error_log: None,
            prometheus_enabled: true,
            prometheus_path: "/metrics".into(),
            log_level: "info".into(),
        }
    }
}

// ─────────────────────────────────────────────
// 站点配置
// ─────────────────────────────────────────────

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

    /// Location 路由规则列表（按配置顺序，内部会按优先级重新排序）
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

    /// 是否作为 fallback 站点（未匹配到其他站点时使用此站点响应）
    /// false（默认）= 不匹配时返回 404/421；true = 作为兜底站点
    #[serde(default)]
    pub fallback: bool,

    /// 站点级 gzip 覆盖（不设则继承全局 global.gzip）
    #[serde(default)]
    pub gzip: Option<bool>,

    /// 站点级 gzip 压缩等级覆盖（不设则继承全局）
    #[serde(default)]
    pub gzip_comp_level: Option<u32>,

    /// 是否启用 WebSocket 支持（反代 ws:// 时需要）
    /// 默认 true，设为 false 可明确禁止升级 WebSocket
    #[serde(default = "default_true")]
    pub websocket: bool,
}

// ─────────────────────────────────────────────
// TLS 配置
// ─────────────────────────────────────────────

/// TLS / HTTPS 配置
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct TlsConfig {
    /// 是否使用 ACME 自动申请证书
    #[serde(default)]
    pub acme: bool,

    /// ACME 注册邮箱
    #[serde(default)]
    pub acme_email: Option<String>,

    /// 手动指定证书文件路径（单证书，与 acme / certs 二选一）
    #[serde(default)]
    pub cert: Option<PathBuf>,

    /// 手动指定私钥文件路径（单证书）
    #[serde(default)]
    pub key: Option<PathBuf>,

    /// 多证书列表（优先级高于 cert/key）
    /// SniResolver 按客户端 ClientHello 签名方案自动选最优证书
    #[serde(default)]
    pub certs: Vec<CertKeyPair>,

    /// 最低 TLS 版本（"tls1.2" / "tls1.3"，默认 tls1.2）
    #[serde(default = "default_tls_min_version")]
    pub min_version: String,

    /// 最高 TLS 版本（"tls1.2" / "tls1.3"，默认 tls1.3）
    #[serde(default = "default_tls_max_version")]
    pub max_version: String,
}

/// 证书/私钥文件对（用于多证书配置）
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CertKeyPair {
    pub cert: PathBuf,
    pub key: PathBuf,
}

// ─────────────────────────────────────────────
// HSTS 配置
// ─────────────────────────────────────────────

/// HSTS（HTTP Strict Transport Security）配置
///
/// 浏览器收到此响应头后，在 max_age 秒内强制使用 HTTPS 访问该域名
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HstsConfig {
    /// HSTS max-age（秒），0 = 禁用 HSTS（删除浏览器记录）
    /// 推荐生产值：31536000（1年）
    #[serde(default = "default_hsts_max_age")]
    pub max_age: u64,

    /// 是否包含 includeSubDomains 指令
    #[serde(default)]
    pub include_sub_domains: bool,

    /// 是否包含 preload 指令（提交到浏览器预加载列表前请确认已满足条件）
    #[serde(default)]
    pub preload: bool,
}

impl Default for HstsConfig {
    fn default() -> Self {
        Self {
            max_age: 31_536_000,
            include_sub_domains: false,
            preload: false,
        }
    }
}

// ─────────────────────────────────────────────
// FastCGI 配置
// ─────────────────────────────────────────────

/// FastCGI（PHP-FPM）配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FastCgiConfig {
    /// Unix Socket 路径（与 host/port 二选一，优先使用 socket）
    #[serde(default)]
    pub socket: Option<PathBuf>,

    /// TCP 主机
    #[serde(default)]
    pub host: Option<String>,

    /// TCP 端口
    #[serde(default)]
    pub port: Option<u16>,

    /// 连接池大小
    #[serde(default = "default_pool_size")]
    pub pool_size: usize,

    /// 连接超时（秒）
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: u64,

    /// 读取超时（秒）
    #[serde(default = "default_read_timeout")]
    pub read_timeout: u64,
}

// ─────────────────────────────────────────────
// Location 配置
// ─────────────────────────────────────────────

/// Location 路由规则
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LocationConfig {
    /// 路径匹配表达式（支持 = /path、^~ /prefix、~ regex、/prefix）
    pub path: String,

    /// 处理器类型
    pub handler: HandlerType,

    /// 覆盖该 Location 的根目录
    #[serde(default)]
    pub root: Option<PathBuf>,

    /// 使用的上游组名称（handler = reverse_proxy 时必填）
    #[serde(default)]
    pub upstream: Option<String>,

    /// Cache-Control 响应头覆盖
    #[serde(default)]
    pub cache_control: Option<String>,

    /// 直接返回状态码（健康检查等）
    #[serde(default)]
    pub return_code: Option<u16>,

    /// WebSocket 最大并发连接数
    #[serde(default)]
    pub max_connections: Option<usize>,

    /// 去掉上游 Set-Cookie 里的 Secure 标志
    /// 适用场景：HTTP 代理 HTTPS 上游时，防止浏览器拒绝存储 Secure Cookie
    /// 等价于 Nginx proxy_cookie_flags ~ Secure drop;
    #[serde(default)]
    pub strip_cookie_secure: bool,

    /// 替换 Set-Cookie 里的 Domain 属性
    /// 限制小数点前的字符串为客户端访问地址
    /// 等价于 Nginx proxy_cookie_domain upstream_host client_host
    #[serde(default)]
    pub proxy_cookie_domain: Option<String>,

    /// Location 响应头中上游 URL 替换为客户端 URL
    /// 格式："https://upstream_host" → "http://client_host"
    /// 等价于 Nginx proxy_redirect https://172.19.0.254 http://172.19.0.1;
    /// 不设则不替换
    #[serde(default)]
    pub proxy_redirect_from: Option<String>,

    /// proxy_redirect 的目标地址（客户端访问的 URL 前缀）
    #[serde(default)]
    pub proxy_redirect_to: Option<String>,
}

/// 请求处理器类型
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HandlerType {
    /// 静态文件服务
    Static,
    /// PHP / FastCGI
    Fastcgi,
    /// WebSocket
    Websocket,
    /// 反向代理
    ReverseProxy,
}

// ─────────────────────────────────────────────
// Rewrite 规则
// ─────────────────────────────────────────────

/// 单条 Rewrite / 伪静态规则
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RewriteRule {
    /// 匹配的正则模式
    pub pattern: String,

    /// 替换目标（支持 $1 $2 捕获组）
    pub target: String,

    /// 行为标志：last / break / redirect(302) / permanent(301)
    #[serde(default = "default_rewrite_flag")]
    pub flag: RewriteFlag,

    /// 触发条件（可选）：!-f 文件不存在，!-d 目录不存在
    #[serde(default)]
    pub condition: Option<String>,
}

/// Rewrite 行为标志
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RewriteFlag {
    /// 重写后重新匹配 location（不跳出循环）
    Last,
    /// 重写后停止处理后续 rewrite
    Break,
    /// 302 临时重定向
    Redirect,
    /// 301 永久重定向
    Permanent,
}

// ─────────────────────────────────────────────
// 反向代理 / 上游配置
// ─────────────────────────────────────────────

/// 上游服务器组配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UpstreamConfig {
    /// 组名（在 LocationConfig.upstream 中引用）
    pub name: String,

    /// 负载均衡策略
    #[serde(default)]
    pub strategy: LoadBalanceStrategy,

    /// 健康检查配置
    #[serde(default)]
    pub health_check: Option<HealthCheckConfig>,

    /// 节点列表
    pub nodes: Vec<UpstreamNode>,
}

/// 负载均衡策略
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoadBalanceStrategy {
    /// 轮询（默认）
    #[default]
    RoundRobin,
    /// 加权轮询
    Weighted,
    /// 最少连接
    LeastConn,
    /// 客户端 IP 哈希
    IpHash,
}

/// 单个上游节点
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UpstreamNode {
    /// 节点地址（host:port）
    pub addr: String,

    /// 权重（用于 Weighted 策略）
    #[serde(default = "default_weight")]
    pub weight: u32,

    /// 是否使用 TLS 连接上游（true = HTTPS/TLS，false = HTTP 明文）
    #[serde(default)]
    pub tls: bool,

    /// TLS SNI 主机名（不设则使用 addr 的 host 部分）
    #[serde(default)]
    pub tls_sni: Option<String>,

    /// 是否跳过上游证书验证（仅用于内网自签名证书，生产慎用）
    #[serde(default)]
    pub tls_insecure: bool,

    /// 发送给上游的 Host 头（不设则使用 addr 的 host 部分）
    /// 用于防止上游因 Host 不匹配而重定向
    #[serde(default)]
    pub upstream_host: Option<String>,
}

/// 健康检查配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HealthCheckConfig {
    /// 是否启用
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// 检查间隔（秒）
    #[serde(default = "default_hc_interval")]
    pub interval: u64,

    /// 超时（秒）
    #[serde(default = "default_hc_timeout")]
    pub timeout: u64,

    /// 检查路径
    #[serde(default = "default_hc_path")]
    pub path: String,
}

// ─────────────────────────────────────────────
// 限流配置
// ─────────────────────────────────────────────

/// 站点限流总配置
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct RateLimitConfig {
    /// 限流规则列表
    #[serde(default)]
    pub rules: Vec<RateLimitRule>,
}

/// 单条限流规则
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RateLimitRule {
    /// 限流维度
    pub dimension: RateLimitDimension,

    /// 稳定速率（每秒请求数）
    pub rate: u64,

    /// 突发容量（令牌桶上限）
    #[serde(default)]
    pub burst: u64,

    /// 路径匹配模式（dimension = path 时使用）
    #[serde(default)]
    pub path_pattern: Option<String>,

    /// Header 名称（dimension = header 时使用）
    #[serde(default)]
    pub header_name: Option<String>,
}

/// 限流维度
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RateLimitDimension {
    /// 按客户端 IP
    Ip,
    /// 按请求路径
    Path,
    /// 按指定 Header 值
    Header,
    /// 按 User-Agent
    UserAgent,
}

// ─────────────────────────────────────────────
// serde 默认值辅助函数
// ─────────────────────────────────────────────

fn default_true() -> bool { true }
fn default_hsts_max_age() -> u64 { 31_536_000 }
fn default_http_ports() -> Vec<u16> { vec![80] }
fn default_index() -> Vec<String> { vec!["index.html".into(), "index.htm".into()] }
fn default_pool_size() -> usize { 32 }
fn default_connect_timeout() -> u64 { 5 }
fn default_read_timeout() -> u64 { 30 }
fn default_weight() -> u32 { 1 }
fn default_hc_interval() -> u64 { 10 }
fn default_hc_timeout() -> u64 { 3 }
fn default_hc_path() -> String { "/health".into() }
fn default_prometheus_path() -> String { "/metrics".into() }
fn default_log_level() -> String { "info".into() }
fn default_rewrite_flag() -> RewriteFlag { RewriteFlag::Last }
fn default_worker_connections() -> usize { 51200 }
fn default_keepalive_timeout() -> u64 { 60 }
fn default_client_max_body_size() -> usize { 50 }
fn default_client_header_buffer_size() -> usize { 32 }
fn default_client_body_buffer_size() -> usize { 512 }
fn default_gzip_min_length() -> usize { 1 }
fn default_gzip_comp_level() -> u32 { 5 }
fn default_tls_min_version() -> String { "tls1.2".into() }
fn default_tls_max_version() -> String { "tls1.3".into() }

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
