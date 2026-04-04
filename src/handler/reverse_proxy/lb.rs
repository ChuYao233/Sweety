//! 负载均衡模块
//! 负责：上游节点状态管理、负载均衡策略、连接池注册表、健康检查后台任务

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::config::model::{LoadBalanceStrategy, UpstreamAddr, UpstreamConfig, UpstreamNode};
use super::circuit_breaker::CircuitBreaker;

// ─────────────────────────────────────────────
// 节点状态
// ─────────────────────────────────────────────

/// 单个上游节点运行时状态
pub struct NodeState {
    pub addr: UpstreamAddr,
    pub weight: u32,
    /// 健康标志（1 = 健康，0 = 不健康）
    pub healthy: AtomicU32,
    /// 当前活跃连接数（least_conn 策略使用）
    pub active_connections: AtomicU32,
    /// 连续失败次数（超过阈値标记不健康）
    pub fail_count: AtomicU32,
    /// 是否用 TLS 连接上游
    pub tls: bool,
    /// TLS SNI 主机名
    pub tls_sni: String,
    /// 跳过上游 TLS 证书验证（内网自签名证书用）
    pub tls_insecure: bool,
    /// 发给上游的 Host 头（不设则透传客户端 Host）
    pub upstream_host: Option<String>,
    /// 是否用 HTTP/2 连接上游（h2c 或 h2 over TLS）
    pub http2: bool,
    /// 向上游发送 PROXY protocol 头（0=不发送，1=v1文本，2=v2二进制）
    pub send_proxy_protocol: u8,
    /// 断路器（None = 未配置，则全程无概履开销）
    pub circuit_breaker: Option<CircuitBreaker>,
}

impl std::fmt::Debug for NodeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeState")
            .field("addr", &self.addr)
            .field("healthy", &self.healthy.load(Ordering::Relaxed))
            .field("active_connections", &self.active_connections.load(Ordering::Relaxed))
            .field("circuit_breaker_open", &self.circuit_breaker.as_ref().map(|cb| cb.is_open()))
            .finish()
    }
}

impl NodeState {
    pub fn new(node: &UpstreamNode, cb_cfg: Option<&crate::config::model::CircuitBreakerConfig>) -> Self {
        // TCP 地址取 host 部分作 SNI；Unix socket 无意义，默认 "localhost"
        let host_part = match &node.addr {
            UpstreamAddr::Tcp(s) => s.split(':').next().unwrap_or(s).to_string(),
            #[cfg(unix)]
            UpstreamAddr::Unix(_) => "localhost".to_string(),
        };
        let sni = node.tls_sni.clone().unwrap_or_else(|| host_part.clone());
        let circuit_breaker = cb_cfg.map(|c| {
            CircuitBreaker::new(c.max_failures, c.window_secs, c.fail_timeout)
        });
        Self {
            addr: node.addr.clone(),
            weight: node.weight,
            healthy: AtomicU32::new(1),
            active_connections: AtomicU32::new(0),
            fail_count: AtomicU32::new(0),
            tls: node.tls,
            tls_sni: sni,
            tls_insecure: node.tls_insecure,
            upstream_host: node.upstream_host.clone(),
            http2: node.http2,
            send_proxy_protocol: node.send_proxy_protocol,
            circuit_breaker,
        }
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed) == 1
    }

    /// 可用 = 健康 + 断路器允许通过
    #[inline(always)]
    pub fn is_available(&self) -> bool {
        if !self.is_healthy() { return false; }
        match &self.circuit_breaker {
            None     => true,
            Some(cb) => cb.allow(),
        }
    }

    pub fn mark_unhealthy(&self) {
        self.healthy.store(0, Ordering::Relaxed);
        warn!("上游节点 {} 标记为不健康", self.addr.as_str());
    }

    pub fn mark_healthy(&self) {
        self.healthy.store(1, Ordering::Relaxed);
        self.fail_count.store(0, Ordering::Relaxed);
        debug!("上游节点 {} 恢复健康", self.addr.as_str());
    }

    /// 记录一次成功（含断路器）
    #[inline(always)]
    pub fn record_success(&self) {
        self.fail_count.store(0, Ordering::Relaxed);
        if let Some(cb) = &self.circuit_breaker {
            cb.record_success();
        }
    }

    /// 记录一次失败（含断路器）
    #[inline(always)]
    pub fn record_failure(&self) {
        let count = self.fail_count.fetch_add(1, Ordering::Relaxed) + 1;
        if count >= 3 {
            self.mark_unhealthy();
        }
        if let Some(cb) = &self.circuit_breaker {
            cb.record_failure();
        }
    }
}

// ─────────────────────────────────────────────
// 上游池
// ─────────────────────────────────────────────

/// 上游节点组，对应配置中的一个 upstream 块
#[derive(Debug)]
pub struct UpstreamPool {
    pub nodes: Vec<Arc<NodeState>>,
    strategy: LoadBalanceStrategy,
    rr_counter: AtomicUsize,
    /// 单连接最大复用请求数（0 = 不限制）
    pub keepalive_requests: u64,
    /// 连接最大复用时间（秒，0 = 不限制）
    pub keepalive_time: u64,
    /// 每节点最大空闲连接数（0 = 用全局默认 32）
    pub keepalive_max_idle: usize,
    /// 连接上游超时（秒）
    pub connect_timeout: u64,
    /// 读取上游响应超时（秒）
    pub read_timeout: u64,
    /// 向上游写入超时（秒）
    pub write_timeout: u64,
    /// 失败重试次数
    pub retry: u32,
    /// 重试等待时间（秒）
    pub retry_timeout: u64,
}

impl UpstreamPool {
    /// 从配置构建上游池
    pub fn from_config(cfg: &UpstreamConfig) -> Self {
        let cb_cfg = cfg.circuit_breaker.as_ref();
        let nodes = cfg.nodes.iter().map(|n| Arc::new(NodeState::new(n, cb_cfg))).collect();
        Self {
            nodes,
            strategy: cfg.strategy.clone(),
            rr_counter: AtomicUsize::new(0),
            keepalive_requests: cfg.keepalive_requests,
            keepalive_time: cfg.keepalive_time,
            keepalive_max_idle: cfg.keepalive,
            connect_timeout: cfg.connect_timeout,
            read_timeout: cfg.read_timeout,
            write_timeout: cfg.write_timeout,
            retry: cfg.retry,
            retry_timeout: cfg.retry_timeout,
        }
    }

    /// 根据策略选出一个可用节点（健康 + 断路器未开路）
    pub fn pick(&self, client_ip: Option<&str>) -> Option<Arc<NodeState>> {
        let avail_count = self.nodes.iter().filter(|n| n.is_available()).count();
        if avail_count == 0 {
            // 全部断路 → 降级：允许选健康节点（让断路器进入 HalfOpen 探测）
            let healthy_count = self.nodes.iter().filter(|n| n.is_healthy()).count();
            if healthy_count == 0 { return None; }
            let idx = self.rr_counter.fetch_add(1, Ordering::Relaxed) % healthy_count;
            return self.nodes.iter().filter(|n| n.is_healthy()).nth(idx).cloned();
        }

        match self.strategy {
            LoadBalanceStrategy::RoundRobin => {
                let idx = self.rr_counter.fetch_add(1, Ordering::Relaxed) % avail_count;
                self.nodes.iter().filter(|n| n.is_available()).nth(idx).cloned()
            }
            LoadBalanceStrategy::Weighted => {
                let total_weight: u32 = self.nodes.iter()
                    .filter(|n| n.is_available()).map(|n| n.weight).sum();
                if total_weight == 0 {
                    return self.nodes.iter().find(|n| n.is_available()).cloned();
                }
                let target = (self.rr_counter.fetch_add(1, Ordering::Relaxed) as u32) % total_weight;
                let mut cumulative = 0u32;
                for node in self.nodes.iter().filter(|n| n.is_available()) {
                    cumulative += node.weight;
                    if target < cumulative { return Some(node.clone()); }
                }
                self.nodes.iter().filter(|n| n.is_available()).last().cloned()
            }
            LoadBalanceStrategy::LeastConn => self.nodes.iter()
                .filter(|n| n.is_available())
                .min_by_key(|n| n.active_connections.load(Ordering::Relaxed))
                .cloned(),
            LoadBalanceStrategy::IpHash => {
                let hash = simple_hash(client_ip.unwrap_or("0.0.0.0"));
                let idx = hash % avail_count;
                self.nodes.iter().filter(|n| n.is_available()).nth(idx).cloned()
            }
        }
    }
}

fn simple_hash(s: &str) -> usize {
    let mut h: usize = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as usize);
    }
    h
}

// ─────────────────────────────────────────────
// 全局注册表
// ─────────────────────────────────────────────

/// 运行时上游池注册表（按站点/上游名索引）
#[derive(Default)]
pub struct UpstreamRegistry {
    pools: RwLock<HashMap<String, Arc<UpstreamPool>>>,
}

impl UpstreamRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn register(&self, key: String, pool: UpstreamPool) {
        self.pools.write().await.insert(key, Arc::new(pool));
    }

    pub async fn get(&self, key: &str) -> Option<Arc<UpstreamPool>> {
        self.pools.read().await.get(key).cloned()
    }
}

// ─────────────────────────────────────────────
// 健康检查
// ─────────────────────────────────────────────

/// 主动健康检查后台任务（每 interval_secs 秒探活一次）
pub async fn health_check_task(pool: Arc<UpstreamPool>, check_path: String, interval_secs: u64) {
    loop {
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
        for node in &pool.nodes {
            let path     = check_path.clone();
            let use_tls  = node.tls;
            let sni      = node.tls_sni.clone();
            let insecure = node.tls_insecure;
            let addr_display = node.addr.as_str().to_string();

            let result = tokio::time::timeout(
                Duration::from_secs(5),
                super::conn::probe_health(&node.addr, &path, use_tls, &sni, insecure),
            ).await;

            match result {
                Ok(Ok(code)) if (200..300).contains(&code) => node.mark_healthy(),
                Ok(Ok(code)) => {
                    warn!("健康检查 {} 返回 {}", addr_display, code);
                    node.fail_count.fetch_add(1, Ordering::Relaxed);
                    if node.fail_count.load(Ordering::Relaxed) >= 3 {
                        node.mark_unhealthy();
                    }
                }
                Ok(Err(e)) => { warn!("健康检查 {} 失败: {}", addr_display, e); node.mark_unhealthy(); }
                Err(_)     => { warn!("健康检查 {} 超时", addr_display); node.mark_unhealthy(); }
            }
        }
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::UpstreamNode;

    fn make_pool(strategy: LoadBalanceStrategy, nodes: &[(&str, u32)]) -> UpstreamPool {
        UpstreamPool {
            nodes: nodes.iter().map(|(addr, weight)| {
                Arc::new(NodeState::new(&UpstreamNode {
                    addr: UpstreamAddr::parse(addr),
                    weight: *weight,
                    tls: false,
                    tls_sni: None,
                    tls_insecure: false,
                    upstream_host: None,
                    http2: false,
                    send_proxy_protocol: 0,
                }, None))
            }).collect(),
            strategy,
            rr_counter: AtomicUsize::new(0),
            keepalive_requests: 0,
            keepalive_time: 0,
            keepalive_max_idle: 32,
            connect_timeout: 10,
            read_timeout: 60,
            write_timeout: 60,
            retry: 0,
            retry_timeout: 0,
        }
    }

    #[test]
    fn test_round_robin() {
        let pool = make_pool(LoadBalanceStrategy::RoundRobin, &[("a:80", 1), ("b:80", 1), ("c:80", 1)]);
        let addrs: Vec<String> = (0..6).map(|_| pool.pick(None).unwrap().addr.as_str().to_string()).collect();
        assert_eq!(&addrs[..3], &["a:80", "b:80", "c:80"]);
        assert_eq!(&addrs[3..], &["a:80", "b:80", "c:80"]);
    }

    #[test]
    fn test_unhealthy_skipped() {
        let pool = make_pool(LoadBalanceStrategy::RoundRobin, &[("a:80", 1), ("b:80", 1)]);
        pool.nodes[0].mark_unhealthy();
        for _ in 0..5 {
            assert_eq!(pool.pick(None).unwrap().addr.as_str(), "b:80");
        }
    }

    #[test]
    fn test_all_unhealthy_returns_none() {
        let pool = make_pool(LoadBalanceStrategy::RoundRobin, &[("a:80", 1)]);
        pool.nodes[0].mark_unhealthy();
        assert!(pool.pick(None).is_none());
    }

    #[test]
    fn test_least_conn() {
        let pool = make_pool(LoadBalanceStrategy::LeastConn, &[("a:80", 1), ("b:80", 1)]);
        pool.nodes[0].active_connections.store(5, Ordering::Relaxed);
        pool.nodes[1].active_connections.store(1, Ordering::Relaxed);
        assert_eq!(pool.pick(None).unwrap().addr.as_str(), "b:80");
    }

    #[test]
    fn test_ip_hash_consistent() {
        let pool = make_pool(LoadBalanceStrategy::IpHash, &[("a:80", 1), ("b:80", 1), ("c:80", 1)]);
        let ip = "192.168.1.100";
        let first = pool.pick(Some(ip)).unwrap().addr.as_str().to_string();
        for _ in 0..10 {
            assert_eq!(pool.pick(Some(ip)).unwrap().addr.as_str(), first);
        }
    }
}
