//! 负载均衡模块
//! 负责：上游节点状态管理、负载均衡策略、连接池注册表、健康检查后台任务

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::config::model::{LoadBalanceStrategy, UpstreamConfig, UpstreamNode};

// ─────────────────────────────────────────────
// 节点状态
// ─────────────────────────────────────────────

/// 单个上游节点运行时状态
#[derive(Debug)]
pub struct NodeState {
    pub addr: String,
    pub weight: u32,
    /// 健康标志（1 = 健康，0 = 不健康）
    pub healthy: AtomicU32,
    /// 当前活跃连接数（least_conn 策略使用）
    pub active_connections: AtomicU32,
    /// 连续失败次数（超过阈值标记不健康）
    pub fail_count: AtomicU32,
    /// 是否用 TLS 连接上游
    pub tls: bool,
    /// TLS SNI 主机名
    pub tls_sni: String,
    /// 跳过上游 TLS 证书验证（内网自签名证书用）
    pub tls_insecure: bool,
    /// 发给上游的 Host 头（不设则透传客户端 Host）
    pub upstream_host: Option<String>,
}

impl NodeState {
    pub fn new(node: &UpstreamNode) -> Self {
        let host_part = node.addr.split(':').next().unwrap_or(&node.addr).to_string();
        let sni = node.tls_sni.clone().unwrap_or_else(|| host_part.clone());
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
        }
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed) == 1
    }

    pub fn mark_unhealthy(&self) {
        self.healthy.store(0, Ordering::Relaxed);
        warn!("上游节点 {} 标记为不健康", self.addr);
    }

    pub fn mark_healthy(&self) {
        self.healthy.store(1, Ordering::Relaxed);
        self.fail_count.store(0, Ordering::Relaxed);
        debug!("上游节点 {} 恢复健康", self.addr);
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
}

impl UpstreamPool {
    /// 从配置构建上游池
    pub fn from_config(cfg: &UpstreamConfig) -> Self {
        let nodes = cfg.nodes.iter().map(|n| Arc::new(NodeState::new(n))).collect();
        Self {
            nodes,
            strategy: cfg.strategy.clone(),
            rr_counter: AtomicUsize::new(0),
        }
    }

    /// 根据策略选出一个健康节点
    /// 直接遍历 nodes，跳过不健康节点，零堆分配（不再收集 healthy Vec）
    pub fn pick(&self, client_ip: Option<&str>) -> Option<Arc<NodeState>> {
        let healthy_count = self.nodes.iter().filter(|n| n.is_healthy()).count();
        if healthy_count == 0 {
            return None;
        }

        match self.strategy {
            LoadBalanceStrategy::RoundRobin => {
                // 轮询：在 nodes 里选第 N 个健康节点
                let idx = self.rr_counter.fetch_add(1, Ordering::Relaxed) % healthy_count;
                self.nodes.iter().filter(|n| n.is_healthy()).nth(idx).cloned()
            }
            LoadBalanceStrategy::Weighted => {
                let total_weight: u32 = self.nodes.iter()
                    .filter(|n| n.is_healthy())
                    .map(|n| n.weight)
                    .sum();
                if total_weight == 0 {
                    return self.nodes.iter().find(|n| n.is_healthy()).cloned();
                }
                let target = (self.rr_counter.fetch_add(1, Ordering::Relaxed) as u32) % total_weight;
                let mut cumulative = 0u32;
                for node in self.nodes.iter().filter(|n| n.is_healthy()) {
                    cumulative += node.weight;
                    if target < cumulative {
                        return Some(node.clone());
                    }
                }
                self.nodes.iter().filter(|n| n.is_healthy()).last().cloned()
            }
            LoadBalanceStrategy::LeastConn => self.nodes.iter()
                .filter(|n| n.is_healthy())
                .min_by_key(|n| n.active_connections.load(Ordering::Relaxed))
                .cloned(),
            LoadBalanceStrategy::IpHash => {
                let hash = simple_hash(client_ip.unwrap_or("0.0.0.0"));
                let idx = hash % healthy_count;
                self.nodes.iter().filter(|n| n.is_healthy()).nth(idx).cloned()
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
            let addr     = node.addr.clone();
            let path     = check_path.clone();
            let use_tls  = node.tls;
            let sni      = node.tls_sni.clone();
            let insecure = node.tls_insecure;

            let result = tokio::time::timeout(
                Duration::from_secs(5),
                super::conn::probe_health(&addr, &path, use_tls, &sni, insecure),
            ).await;

            match result {
                Ok(Ok(code)) if (200..300).contains(&code) => node.mark_healthy(),
                Ok(Ok(code)) => {
                    warn!("健康检查 {} 返回 {}", addr, code);
                    node.fail_count.fetch_add(1, Ordering::Relaxed);
                    if node.fail_count.load(Ordering::Relaxed) >= 3 {
                        node.mark_unhealthy();
                    }
                }
                Ok(Err(e)) => { warn!("健康检查 {} 失败: {}", addr, e); node.mark_unhealthy(); }
                Err(_)     => { warn!("健康检查 {} 超时", addr); node.mark_unhealthy(); }
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
                    addr: addr.to_string(),
                    weight: *weight,
                    tls: false,
                    tls_sni: None,
                    tls_insecure: false,
                    upstream_host: None,
                }))
            }).collect(),
            strategy,
            rr_counter: AtomicUsize::new(0),
        }
    }

    #[test]
    fn test_round_robin() {
        let pool = make_pool(LoadBalanceStrategy::RoundRobin, &[("a:80", 1), ("b:80", 1), ("c:80", 1)]);
        let addrs: Vec<String> = (0..6).map(|_| pool.pick(None).unwrap().addr.clone()).collect();
        assert_eq!(&addrs[..3], &["a:80", "b:80", "c:80"]);
        assert_eq!(&addrs[3..], &["a:80", "b:80", "c:80"]);
    }

    #[test]
    fn test_unhealthy_skipped() {
        let pool = make_pool(LoadBalanceStrategy::RoundRobin, &[("a:80", 1), ("b:80", 1)]);
        pool.nodes[0].mark_unhealthy();
        for _ in 0..5 {
            assert_eq!(pool.pick(None).unwrap().addr, "b:80");
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
        assert_eq!(pool.pick(None).unwrap().addr, "b:80");
    }

    #[test]
    fn test_ip_hash_consistent() {
        let pool = make_pool(LoadBalanceStrategy::IpHash, &[("a:80", 1), ("b:80", 1), ("c:80", 1)]);
        let ip = "192.168.1.100";
        let first = pool.pick(Some(ip)).unwrap().addr.clone();
        for _ in 0..10 {
            assert_eq!(pool.pick(Some(ip)).unwrap().addr, first);
        }
    }
}
