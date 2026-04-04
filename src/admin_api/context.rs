//! AdminContext：管理 API 共享上下文

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use crate::config::model::AppConfig;
use crate::dispatcher::vhost::VHostRegistry;
use crate::middleware::metrics::GlobalMetrics;
use crate::server::tls::SniResolver;

/// 管理 API 共享上下文
///
/// 所有字段均为 Arc 引用，与主服务器共享同一份数据，
/// admin API 的读操作不会影响请求处理热路径性能。
#[derive(Clone)]
pub struct AdminContext {
    /// 虚拟主机注册表（站点列表、上游池、Location 等）
    pub registry: Arc<VHostRegistry>,
    /// 全局指标计数器
    pub metrics: Arc<GlobalMetrics>,
    /// 应用配置（热重载原子替换）
    pub cfg: Arc<ArcSwap<AppConfig>>,
    /// 当前活跃连接数
    pub active_connections: Arc<std::sync::atomic::AtomicUsize>,
    /// TLS SNI 解析器（ACME 续期后热重载证书用）
    pub sni_resolvers: HashMap<u16, Arc<SniResolver>>,
    /// Bearer Token（空 = 不鉴权）
    pub token: String,
    /// 监听地址
    pub listen_addr: String,
    /// 服务器启动时间（用于计算 uptime）
    pub start_time: std::time::Instant,
    /// 配置文件路径（热重载 / 持久化用）
    pub config_path: Option<std::path::PathBuf>,
}
