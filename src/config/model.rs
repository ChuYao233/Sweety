//! 配置结构体定义模块
//! 所有配置项对应的 Rust 数据结构，通过 serde 实现多格式反序列化

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

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

    /// 全局最大并发 HTTP 连接数（0 = 不限制，生产建议设为 10000-50000）
    /// 超出时返回 503 Service Unavailable，防止内存爆涨
    #[serde(default)]
    pub max_connections: usize,

    /// Keep-Alive 超时（秒），0 = 禁用（等价 Nginx keepalive_timeout）
    #[serde(default = "default_keepalive_timeout")]
    pub keepalive_timeout: u64,

    /// FastCGI 连接超时（秒，等价 Nginx fastcgi_connect_timeout）
    #[serde(default = "default_fastcgi_connect_timeout")]
    pub fastcgi_connect_timeout: u64,

    /// FastCGI 读取超时（秒，等价 Nginx fastcgi_read_timeout）
    #[serde(default = "default_fastcgi_read_timeout")]
    pub fastcgi_read_timeout: u64,

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

    /// HTTP/2 单连接最大并发流数（等价 Nginx http2_max_concurrent_streams，默认 32）
    /// 超出后服务端发送 GOAWAY，客户端会新开连接
    /// 生产建议：32-64，让多条连接分散到多个 worker 线程并行 TLS 加密
    /// 设过大（如 102400）会导致所有流挤在一条连接上，TLS 串行于单核
    #[serde(default = "default_h2_max_concurrent_streams")]
    pub h2_max_concurrent_streams: u32,

    /// HTTP/2 单连接最大同时在途 handler 数量（默认 0 = 不限制）
    /// 超出后服务端发送 GOAWAY 优雅拒绝新流，等价 Nginx 连接队列限制
    /// 0 表示不限制，依赖 h2_max_concurrent_streams 做协议级流控
    #[serde(default)]
    pub h2_max_pending_per_conn: usize,

    /// HTTP/2 RST 洪水防护：单连接最大并发 reset 流数（默认 200）
    /// h2 crate 默认值为 20，压测场景需要调高
    #[serde(default = "default_h2_max_concurrent_reset_streams")]
    pub h2_max_concurrent_reset_streams: usize,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            worker_threads: 0,
            worker_connections: default_worker_connections(),
            max_connections: 0,
            keepalive_timeout: default_keepalive_timeout(),
            fastcgi_connect_timeout: default_fastcgi_connect_timeout(),
            fastcgi_read_timeout: default_fastcgi_read_timeout(),
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
            h2_max_concurrent_streams: default_h2_max_concurrent_streams(),
            h2_max_pending_per_conn: 0,
            h2_max_concurrent_reset_streams: default_h2_max_concurrent_reset_streams(),
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

    /// 访问日志格式（等价 Nginx log_format）
    /// 不设则使用 access_log_format 指定的格式，再不设则 combined
    /// 支持变量：$remote_addr $method $uri $http_version $status
    ///           $bytes_sent $http_referer $http_user_agent $duration_ms $time_local $site
    /// 特殊格式名："combined"（默认）、"json"
    /// 自定义格式示例: "$remote_addr [$time_local] \"$method $uri\" $status $bytes_sent"
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

    /// 是否强制 HTTP 跳转到 HTTPS（仅当站点配置了 listen_tls 时生效）
    #[serde(default)]
    pub force_https: bool,

    /// 自定义错误页（等价 Nginx error_page 404 /404.html）
    /// key = HTTP 状态码（TOML 内用字符串写法：\"404\")，value = 静态文件路径
    #[serde(default, deserialize_with = "deserialize_u16_map")]
    pub error_pages: HashMap<u16, String>,

    /// 反代响应缓存配置（等价 Nginx proxy_cache）
    #[serde(default)]
    pub proxy_cache: Option<ProxyCacheConfig>,
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

    /// ACME 证书到期前多少天自动续期（默认 30 天）
    /// Let's Encrypt 证书有效期 90 天，建议不要小于 7 天
    #[serde(default = "default_acme_renew_days")]
    pub acme_renew_days_before: u64,

    /// ACME 证书提供商
    /// - "letsencrypt"（默认）：Let's Encrypt 生产环境
    /// - "letsencrypt_staging"：Let's Encrypt 测试环境（不稳耗配额，证书不受信任）
    /// - "zerossl"：ZeroSSL（免费 90 天证书）
    /// - "buypass"：Buypass / LiteSSL（免费，180 天证书）
    /// - 自定义 URL：任何其他支持 ACME 协议的 CA
    #[serde(default = "default_acme_provider")]
    pub acme_provider: String,

    /// ACME 验证方式："http01"（默认） 或 "dns01"
    /// dns01 支持通配符证书（*.example.com），不需要 80 端口可达
    #[serde(default = "default_acme_challenge")]
    pub acme_challenge: String,

    /// DNS provider 配置（dns01 验证时必需）
    #[serde(default)]
    pub dns_provider: Option<DnsProviderConfig>,

    /// 启用的 HTTP 协议列表，序列即优先级（客户端支持时优先应答列表第一个）
    /// 可选属性："h3" / "h2" / "http/1.1"
    /// 不填则默认全开 ["h3", "h2", "http/1.1"]（h3 优先通过 Alt-Svc 广播，TCP 侧 h2 优先）
    /// 示例：
    ///   protocols = ["h2", "http/1.1"]  # 禁用 HTTP/3
    ///   protocols = ["h3", "h2"]        # 禁用 HTTP/1.1
    ///   protocols = ["http/1.1"]        # 仅 H1（极次兼容模式）
    #[serde(default = "default_protocols")]
    pub protocols: Vec<String>,

    /// HTTP/3 QUIC 传输层调优（不填使用内置性能默认值）
    #[serde(default)]
    pub http3: Http3Config,
}
///
/// 对应 quinn::TransportConfig 的关键字段，不配置则使用性能优化的默认值
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Http3Config {
    /// 单连接最大并发双向流数（等价 HTTP/2 max_concurrent_streams，默认 200）
    /// 建议：API 服务设 100，静态资源服务设 300
    #[serde(default = "default_h3_max_concurrent_bidi_streams")]
    pub max_concurrent_bidi_streams: u32,

    /// 单连接最大并发单向流数（默认 100）
    #[serde(default = "default_h3_max_concurrent_uni_streams")]
    pub max_concurrent_uni_streams: u32,

    /// 连接空闲超时（毫秒，默认 30000 = 30s，0 = 禁用）
    /// 客户端无活动超过此时间后服务端主动关闭连接
    #[serde(default = "default_h3_idle_timeout_ms")]
    pub idle_timeout_ms: u64,

    /// Keep-Alive 间隔（毫秒，默认 10000 = 10s，0 = 禁用）
    /// 定期发送 PING 保持连接，避免 NAT/防火墙超时断开
    #[serde(default = "default_h3_keep_alive_interval_ms")]
    pub keep_alive_interval_ms: u64,

    /// 连接级接收窗口（字节，默认 8MB）
    /// 影响整个连接的吞吐量上限，高并发大文件传输建议设 16-64MB
    #[serde(default = "default_h3_receive_window")]
    pub receive_window: u64,

    /// 流级接收窗口（字节，默认 2MB）
    /// 单个 HTTP 请求/响应流的流控窗口
    #[serde(default = "default_h3_stream_receive_window")]
    pub stream_receive_window: u64,

    /// 连接级发送窗口（字节，默认 8MB）
    #[serde(default = "default_h3_send_window")]
    pub send_window: u64,

    /// 是否启用 0-RTT（Early Data，默认 false）
    /// 开启后客户端可在握手前发送请求，减少 RTT，但有重放攻击风险
    /// 仅在内网或低风险场景开启
    #[serde(default)]
    pub enable_0rtt: bool,

    /// MTU 探测（默认 true）：自动发现最优 PMTU，减少分片，提升吞吐
    #[serde(default = "default_true")]
    pub mtu_discovery: bool,
}

impl Default for Http3Config {
    fn default() -> Self {
        Self {
            max_concurrent_bidi_streams: default_h3_max_concurrent_bidi_streams(),
            max_concurrent_uni_streams:  default_h3_max_concurrent_uni_streams(),
            idle_timeout_ms:             default_h3_idle_timeout_ms(),
            keep_alive_interval_ms:      default_h3_keep_alive_interval_ms(),
            receive_window:              default_h3_receive_window(),
            stream_receive_window:       default_h3_stream_receive_window(),
            send_window:                 default_h3_send_window(),
            enable_0rtt:                 false,
            mtu_discovery:               true,
        }
    }
}

/// DNS provider 配置（用于 ACME DNS-01 验证）
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DnsProviderConfig {
    /// Cloudflare DNS API
    Cloudflare {
        /// Cloudflare API Token（推荐）或 Global API Key
        api_token: String,
        /// Zone ID（可选，不填则自动查找）
        #[serde(default)]
        zone_id: Option<String>,
    },
    /// 阿里云 DNS
    Aliyun {
        /// AccessKey ID
        access_key_id: String,
        /// AccessKey Secret
        access_key_secret: String,
    },
    /// 自定义 Shell 脚本（通用展展名）
    /// 论本接受两个参数：域名、TXT 记录内容
    Shell {
        /// 设置 TXT 记录的脚本路径（参数: <domain> <txt_value>）
        set_script: String,
        /// 删除 TXT 记录的脚本路径（参数: <domain>）
        #[serde(default)]
        del_script: Option<String>,
    },
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
    /// 默认不设置（WordPress 缓存如需跳过已登录用户，可设为 ["cookie"]）
    #[serde(default)]
    pub bypass_headers: Vec<String>,
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

    /// 自定义转发请求头（覆盖或新增）
    /// 等价 Nginx proxy_set_header
    /// 示例: [{name="X-Custom", value="foo"}]
    #[serde(default)]
    pub proxy_set_headers: Vec<HeaderOverride>,

    /// 向客户端响应中注入自定义头
    /// 等价 Nginx add_header
    #[serde(default)]
    pub add_headers: Vec<HeaderOverride>,

    /// 按扩展名正则设置缓存规则（等价 Nginx if $uri ~* "..." { expires ... }）
    #[serde(default)]
    pub cache_rules: Vec<CacheRule>,

    /// 直接返回（带 URL 的 return 指令，等价 Nginx return 301 https://...）
    /// 格式: "301 https://example.com$request_uri" 或 "https://example.com"
    #[serde(default)]
    pub return_url: Option<String>,

    /// 直接返回文本内容体（等价 Caddy respond / Nginx return 200 "text"）
    /// 与 return_code 配合使用，不设 return_code 时默认 200
    /// 示例: return_body = "OK"
    #[serde(default)]
    pub return_body: Option<String>,

    /// return_body 的 Content-Type（默认 "text/plain; charset=utf-8"）
    #[serde(default)]
    pub return_content_type: Option<String>,

    /// per-location 并发连接数限制（等价 Nginx limit_conn）
    /// 超出返回 503，0 = 不限制
    #[serde(default)]
    pub limit_conn: usize,

    /// 反代缓冲控制（等价 Nginx proxy_buffering）
    /// false（默认）= 流式转发，不把响应体读入内存，高并发安全
    /// true = 缓冲模式，仅当需要 sub_filter / proxy_cache / URL 替换时才设为 true
    /// 注意：true 时 1000 并发× 1MB 响应 = 1GB 内存，容易 OOM
    #[serde(default)]
    pub proxy_buffering: bool,

    /// 尝试文件列表（等价 Nginx try_files $uri $uri/ /index.html）
    /// 支持: $uri、$uri/、/fallback.html、=404 等
    #[serde(default)]
    pub try_files: Vec<String>,

    /// 响应体内容替换（等价 Nginx sub_filter）
    /// 仅对文本类 Content-Type（html/json/js/text）生效
    #[serde(default)]
    pub sub_filter: Vec<SubFilter>,

    /// 子请求鉴权 URL（等价 Nginx auth_request /auth）
    /// 每个请求先向此 URL 发 GET 子请求；2xx 则继续，非 2xx 则返回 401/403
    /// 鉴权服务可通过响应头传回 X-Auth-User 等信息（会注入原始请求的头中）
    #[serde(default)]
    pub auth_request: Option<String>,

    /// auth_request 失败时返回的状态码（默认 401）
    #[serde(default = "default_auth_failure_status")]
    pub auth_failure_status: u16,

    /// auth_request 时向子请求注入的额外头（等价 auth_request_set）
    /// 格式同 proxy_set_headers
    #[serde(default)]
    pub auth_request_headers: Vec<HeaderOverride>,
}

impl Default for LocationConfig {
    fn default() -> Self {
        Self {
            path: String::new(),
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
            return_body: None,
            return_content_type: None,
            limit_conn: 0,
            proxy_buffering: false,
            try_files: vec![],
            sub_filter: vec![],
            auth_request: None,
            auth_failure_status: 401,
            auth_request_headers: vec![],
        }
    }
}

/// 请求头/响应头覆盖配置
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HeaderOverride {
    /// 头名称
    pub name: String,
    /// 头值（支持变量: $remote_addr, $host, $scheme, $request_uri）
    pub value: String,
}

/// 按扩展名缓存规则
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CacheRule {
    /// 匹配扩展名的正则表达式（如 ".\\.(css|js|png)$"）
    pub pattern: String,
    /// Cache-Control 头值（如 "public, max-age=2592000"）
    pub cache_control: String,
}

/// 响应体内容替换规则（等价 Nginx sub_filter）
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubFilter {
    /// 被替换的字符串或正则（以 `~` 开头表示正则）
    pub pattern: String,
    /// 替换内容（支持 $1 $2 捕获组）
    pub replacement: String,
}

/// 反代响应缓存配置（等价 Nginx proxy_cache）
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProxyCacheConfig {
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

    /// 跳过缓存的请求头（如 Authorization 头存在时不缓存）
    #[serde(default = "default_no_cache_headers")]
    pub bypass_headers: Vec<String>,
}

/// 请求处理器类型
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Default)]
pub enum HandlerType {
    /// 静态文件服务
    #[default]
    #[serde(rename = "static")]
    Static,
    /// PHP / FastCGI
    #[serde(rename = "fastcgi")]
    Fastcgi,
    /// WebSocket
    #[serde(rename = "websocket")]
    Websocket,
    /// 反向代理
    #[serde(rename = "reverse_proxy")]
    ReverseProxy,
    /// gRPC 反向代理（HTTP/2 二进制帧，Content-Type: application/grpc）
    #[serde(rename = "grpc")]
    Grpc,
    /// 插件处理器（handler = "plugin:name" 格式）
    /// 序列化时保留完整字符串，反序列化时从 "plugin:xxx" 解析
    #[serde(untagged, deserialize_with = "deserialize_plugin_handler")]
    Plugin(String),
}

/// 反序列化 plugin:<name> 格式的 handler 字段
fn deserialize_plugin_handler<'de, D>(de: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(de)?;
    if let Some(name) = s.strip_prefix("plugin:") {
        Ok(name.to_string())
    } else {
        Err(serde::de::Error::custom(format!("期望 plugin:<name> 格式，得到: {}", s)))
    }
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

    /// Keepalive 空闲连接池大小（等价 Nginx keepalive N）
    /// 每个 worker 保持的最大空闲连接数，0 = 32（默认）
    #[serde(default)]
    pub keepalive: usize,

    /// 单个连接最大复用请求数（等价 Nginx keepalive_requests）
    /// 达到此数后关闭连接重建，0 = 不限制
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

/// 断路器配置（相比 Nginx max_fails/fail_timeout 增加全局开关能力）
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CircuitBreakerConfig {
    /// 时间窗口内最大失败次数（超过后开路）
    #[serde(default = "default_cb_max_failures")]
    pub max_failures: u32,

    /// 时间窗口大小（秒），其内计算失败次数
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

    /// 是否用 HTTP/2 连接上游（h2c 明文 或 h2 over TLS）
    /// true + tls=false = h2c（HTTP/2 cleartext，上游需支持 h2c prior knowledge）
    /// true + tls=true  = h2 over TLS（ALPN negotiation，常见于 gRPC 上游）
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

    /// 突发容量（令牌桶上限，默认 = rate）
    #[serde(default)]
    pub burst: u64,

    /// nodelay 模式（等价 Nginx limit_req nodelay）
    /// true = burst 内请求立即放行，不排队，超出 burst 才 429
    /// false = 按陈列限速，请求到达过快则等待（默认 true，与 Nginx 行为一致）
    #[serde(default = "default_true")]
    pub nodelay: bool,

    /// 路径匹配模式（dimension = path 时使用）
    #[serde(default)]
    pub path_pattern: Option<String>,

    /// Header 名称（dimension = header 时使用）
    #[serde(default)]
    pub header_name: Option<String>,
}

/// 限流维度
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RateLimitDimension {
    /// 按客户端 IP
    Ip,
    /// 按请求路径
    Path,
    /// 按指定 Header 值
    Header,
    /// 按 User-Agent
    UserAgent,
    /// IP + 路径组合（等价 Nginx $binary_remote_addr$uri）
    IpPath,
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
fn default_h2_max_concurrent_streams() -> u32 { 128 }
fn default_h2_max_concurrent_reset_streams() -> usize { 200 }
fn default_keepalive_timeout() -> u64 { 60 }
fn default_fastcgi_connect_timeout() -> u64 { 5 }
fn default_fastcgi_read_timeout() -> u64 { 60 }
fn default_client_max_body_size() -> usize { 50 }
fn default_client_header_buffer_size() -> usize { 32 }
fn default_client_body_buffer_size() -> usize { 512 }
fn default_gzip_min_length() -> usize { 1 }
fn default_gzip_comp_level() -> u32 { 5 }
fn default_tls_min_version() -> String { "tls1.2".into() }
fn default_tls_max_version() -> String { "tls1.3".into() }
fn default_acme_renew_days() -> u64 { 30 }
fn default_acme_provider() -> String { "letsencrypt".into() }
fn default_acme_challenge() -> String { "http01".into() }
fn default_protocols() -> Vec<String> { vec!["h3".into(), "h2".into(), "http/1.1".into()] }
fn default_cache_max_entries() -> usize { 1000 }
fn default_cache_ttl() -> u64 { 60 }
fn default_cache_statuses() -> Vec<u16> { vec![200, 301, 302] }
fn default_cache_methods() -> Vec<String> { vec!["GET".into(), "HEAD".into()] }
fn default_no_cache_headers() -> Vec<String> { vec!["Authorization".into(), "Cookie".into()] }
fn default_auth_failure_status() -> u16 { 401 }
fn default_proxy_connect_timeout() -> u64 { 10 }
fn default_proxy_read_timeout() -> u64 { 60 }
fn default_proxy_write_timeout() -> u64 { 60 }
fn default_cb_max_failures() -> u32 { 5 }
fn default_cb_window() -> u64 { 60 }
fn default_cb_fail_timeout() -> u64 { 30 }
fn default_h3_max_concurrent_bidi_streams() -> u32 { 200 }
fn default_h3_max_concurrent_uni_streams() -> u32 { 100 }
fn default_h3_idle_timeout_ms() -> u64 { 30_000 }
fn default_h3_keep_alive_interval_ms() -> u64 { 10_000 }
fn default_h3_receive_window() -> u64 { 8 * 1024 * 1024 }        // 8 MB
fn default_h3_stream_receive_window() -> u64 { 2 * 1024 * 1024 } // 2 MB
fn default_h3_send_window() -> u64 { 8 * 1024 * 1024 }           // 8 MB

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
