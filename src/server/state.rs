//! 应用共享状态 AppState 及连接限流守卫 ConnGuard

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use arc_swap::ArcSwap;
use crate::config::model::AppConfig;
use crate::dispatcher::vhost::VHostRegistry;
use crate::handler::reverse_proxy::pool::ConnPool;
use crate::handler::reverse_proxy::upstream_h2::H2UpstreamPools;
use crate::handler::fastcgi_pool::FcgiPool;
use crate::middleware::metrics::GlobalMetrics;
use super::tls::SniResolver;

/// 所有请求共享状态
/// 字段顺序按热路径访问频率排列，提升缓存行命中率
#[derive(Clone)]
pub struct AppState {
    /// 虚拟主机注册表（每请求必访）
    pub registry: Arc<VHostRegistry>,
    /// 全局指标（每请求必访）
    pub metrics: Arc<GlobalMetrics>,
    /// 应用配置（每请求必访，支持热重载原子替换）
    pub cfg: Arc<ArcSwap<AppConfig>>,
    /// 最大并发连接数（0 = 不限制）
    pub max_connections: usize,
    /// 最大请求体字节数（预计算，避免每请求乘法；0 = 不限制）
    pub max_body_bytes: u64,
    /// 当前活跃连接数（原子计数器，无锁）
    pub active_connections: Arc<std::sync::atomic::AtomicUsize>,
    /// 是否有任意站点配置了访问日志（用于 req_start 延迟初始化判断）
    pub any_access_log: bool,
    /// 上游 TCP/TLS 连接池（跨请求复用 idle 连接）
    pub conn_pool: ConnPool,
    /// SNI 证书 Resolver 按端口索引（热重载时原地更新证书，不断连）
    pub sni_resolvers: Arc<HashMap<u16, Arc<SniResolver>>>,
    /// HTTP/2 上游连接池（h2c / h2 over TLS）
    pub h2_pools: Arc<H2UpstreamPools>,
    /// FastCGI 连接池（复用 PHP-FPM 连接）
    pub fcgi_pool: Arc<FcgiPool>,
    /// HTTP/3 QUIC 端口集合（用于注入 Alt-Svc 响应头）
    pub h3_ports: Arc<HashSet<u16>>,
}

/// max_connections / limit_conn RAII 守卫：Drop 时自动减计数器
pub(crate) struct ConnGuard(pub(crate) Arc<std::sync::atomic::AtomicUsize>);

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}
