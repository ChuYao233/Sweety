//! 上游服务器组配置及 FastCGI 配置
//!
//! UpstreamConfig / CircuitBreakerConfig / LoadBalanceStrategy / UpstreamNode / HealthCheckConfig
//! FastCgiConfig / FastCgiCacheConfig

use std::path::PathBuf;
use serde::{Deserialize, Serialize};

fn default_pool_size() -> usize { 32 }
fn default_connect_timeout() -> u64 { 5 }
fn default_read_timeout() -> u64 { 30 }
fn default_weight() -> u32 { 1 }
fn default_hc_interval() -> u64 { 10 }
fn default_hc_timeout() -> u64 { 3 }
fn default_hc_path() -> String { "/health".into() }
fn default_true() -> bool { true }
fn default_proxy_connect_timeout() -> u64 { 10 }
fn default_proxy_read_timeout() -> u64 { 60 }
fn default_proxy_write_timeout() -> u64 { 60 }
fn default_cb_max_failures() -> u32 { 5 }
fn default_cb_window() -> u64 { 60 }
fn default_cb_fail_timeout() -> u64 { 30 }
fn default_cache_max_entries() -> usize { 1000 }
fn default_cache_ttl() -> u64 { 60 }
fn default_cache_statuses() -> Vec<u16> { vec![200, 301, 302] }
fn default_cache_methods() -> Vec<String> { vec!["GET".into(), "HEAD".into()] }

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

    /// Keepalive 空闲连接池大小（等价 Nginx keepalive N）
    #[serde(default)]
    pub keepalive: usize,

    /// 单个连接最大复用请求数（等价 Nginx keepalive_requests）
    #[serde(default)]
    pub keepalive_requests: u64,

    /// 连接最大复用时间（秒，0 = 不限制）
    #[serde(default)]
    pub keepalive_time: u64,

    /// 连接上游超时（秒，等价 Nginx proxy_connect_timeout，默认 10）
    #[serde(default = "default_proxy_connect_timeout")]
    pub connect_timeout: u64,

    /// 读取上游响应超时（秒，等价 Nginx proxy_read_timeout，默认 60）
    #[serde(default = "default_proxy_read_timeout")]
    pub read_timeout: u64,

    /// 向上游写入超时（秒，等价 Nginx proxy_send_timeout，默认 60）
    #[serde(default = "default_proxy_write_timeout")]
    pub write_timeout: u64,

    /// 失败重试次数（0 = 不重试）
    #[serde(default)]
    pub retry: u32,

    /// 重试前等待时间（秒，0 = 立即重试）
    #[serde(default)]
    pub retry_timeout: u64,

    /// 断路器配置
    #[serde(default)]
    pub circuit_breaker: Option<CircuitBreakerConfig>,
}

/// 断路器配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CircuitBreakerConfig {
    /// 时间窗口内最大失败次数（超过后开路）
    #[serde(default = "default_cb_max_failures")]
    pub max_failures: u32,

    /// 时间窗口大小（秒）
    #[serde(default = "default_cb_window")]
    pub window_secs: u64,

    /// 开路后尝试恢复的等待时间（秒，等价 Nginx fail_timeout）
    #[serde(default = "default_cb_fail_timeout")]
    pub fail_timeout: u64,
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

    /// 是否使用 TLS 连接上游
    #[serde(default)]
    pub tls: bool,

    /// TLS SNI 主机名（不设则使用 addr 的 host 部分）
    #[serde(default)]
    pub tls_sni: Option<String>,

    /// 是否跳过上游证书验证
    #[serde(default)]
    pub tls_insecure: bool,

    /// 发送给上游的 Host 头
    #[serde(default)]
    pub upstream_host: Option<String>,

    /// 是否用 HTTP/2 连接上游（h2c 或 h2 over TLS）
    #[serde(default)]
    pub http2: bool,
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

    /// FastCGI 响应缓存（等价 Nginx fastcgi_cache）
    #[serde(default)]
    pub cache: Option<FastCgiCacheConfig>,
}

/// FastCGI 响应缓存配置（对标 Nginx fastcgi_cache）
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FastCgiCacheConfig {
    /// 磁盘缓存目录（不设则只用内存缓存）
    #[serde(default)]
    pub path: Option<PathBuf>,

    /// 内存缓存最大条数（默认 1000）
    #[serde(default = "default_cache_max_entries")]
    pub max_entries: usize,

    /// 缓存有效期（秒，默认 60）
    #[serde(default = "default_cache_ttl")]
    pub ttl: u64,

    /// 可缓存的 HTTP 状态码（默认 [200, 301, 302]）
    #[serde(default = "default_cache_statuses")]
    pub cacheable_statuses: Vec<u16>,

    /// 可缓存的 HTTP 方法（默认 ["GET", "HEAD"]）
    #[serde(default = "default_cache_methods")]
    pub cacheable_methods: Vec<String>,

    /// 跳过缓存的请求头（如 Cookie 存在时不缓存）
    #[serde(default)]
    pub bypass_headers: Vec<String>,
}
