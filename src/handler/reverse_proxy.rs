//! 反向代理处理器
//! 支持协议：HTTP / HTTPS / WS / WSS
//! TLS：tokio-rustls，支持 tls_insecure（跳过证书验证，用于内网自签名证书）
//! 负载均衡：轮询 / 加权 / 最少连接 / IP 哈希
//! 健康检查：主动 TCP 探活

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::Result;
use rustls::ClientConfig as RustlsClientConfig;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio_rustls::TlsConnector;
use tracing::{debug, error, warn};
use xitca_web::{
    body::ResponseBody,
    http::{StatusCode, WebResponse, header::{CONTENT_TYPE, HeaderValue}},
    WebContext,
};

use crate::config::model::{LoadBalanceStrategy, LocationConfig, UpstreamConfig, UpstreamNode};
use crate::dispatcher::vhost::SiteInfo;
use crate::server::http::AppState;

// ─────────────────────────────────────────────
// TLS 客户端配置构建
// ─────────────────────────────────────────────

/// 构建 TLS 客户端配置
///
/// - `insecure = false`：正常验证服务端证书（生产推荐）
/// - `insecure = true`：跳过证书验证（内网自签名 / 开发调试用）
fn build_tls_client_config(insecure: bool) -> Arc<RustlsClientConfig> {
    if insecure {
        // 自定义 ServerCertVerifier，跳过所有验证
        #[derive(Debug)]
        struct NoVerifier;

        impl rustls::client::danger::ServerCertVerifier for NoVerifier {
            fn verify_server_cert(
                &self,
                _end_entity: &rustls::pki_types::CertificateDer<'_>,
                _intermediates: &[rustls::pki_types::CertificateDer<'_>],
                _server_name: &rustls::pki_types::ServerName<'_>,
                _ocsp_response: &[u8],
                _now: rustls::pki_types::UnixTime,
            ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
                Ok(rustls::client::danger::ServerCertVerified::assertion())
            }

            fn verify_tls12_signature(
                &self,
                _message: &[u8],
                _cert: &rustls::pki_types::CertificateDer<'_>,
                _dss: &rustls::DigitallySignedStruct,
            ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
                Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
            }

            fn verify_tls13_signature(
                &self,
                _message: &[u8],
                _cert: &rustls::pki_types::CertificateDer<'_>,
                _dss: &rustls::DigitallySignedStruct,
            ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
                Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
            }

            fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
                rustls::crypto::ring::default_provider()
                    .signature_verification_algorithms
                    .supported_schemes()
            }
        }

        let cfg = RustlsClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth();
        Arc::new(cfg)
    } else {
        // 使用系统/webpki 根证书（验证服务端证书）
        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let cfg = RustlsClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();
        Arc::new(cfg)
    }
}

/// 对已建立的 TCP 连接做 TLS 握手，返回加密流
async fn tls_connect(
    tcp: TcpStream,
    sni: &str,
    insecure: bool,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let config = build_tls_client_config(insecure);
    let connector = TlsConnector::from(config);
    let server_name = rustls::pki_types::ServerName::try_from(sni.to_string())
        .map_err(|e| anyhow::anyhow!("无效的 TLS SNI '{}': {}", sni, e))?;
    let stream = connector.connect(server_name, tcp).await
        .map_err(|e| anyhow::anyhow!("TLS 握手失败 ({}): {}", sni, e))?;
    Ok(stream)
}

// ─────────────────────────────────────────────
// 节点健康状态
// ─────────────────────────────────────────────

/// 单个上游节点运行时状态
#[derive(Debug)]
pub struct NodeState {
    /// 节点地址（host:port）
    pub addr: String,
    /// 权重
    pub weight: u32,
    /// 是否健康（false = 暂时从轮询中移除）
    pub healthy: AtomicU32,
    /// 当前活跃连接数（用于 least_conn 策略）
    pub active_connections: AtomicU32,
    /// 连续失败次数
    pub fail_count: AtomicU32,
    /// 是否使用 TLS 连接上游（HTTPS 上游）
    pub tls: bool,
    /// TLS SNI 主机名
    pub tls_sni: String,
    /// 跳过上游证书验证（内网自签名证书用）
    pub tls_insecure: bool,
}

impl NodeState {
    pub fn new(node: &UpstreamNode) -> Self {
        // 提取 SNI：优先用配置的 tls_sni，否则取 addr 的主机名部分
        let sni = node.tls_sni.clone().unwrap_or_else(|| {
            node.addr.split(':').next().unwrap_or(&node.addr).to_string()
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
// 上游节点组
// ─────────────────────────────────────────────

/// 上游节点组（对应配置中的 UpstreamConfig）
pub struct UpstreamPool {
    /// 节点列表
    nodes: Vec<Arc<NodeState>>,
    /// 负载均衡策略
    strategy: LoadBalanceStrategy,
    /// 轮询计数器（round_robin / weighted 使用）
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

    /// 根据策略选择一个健康节点
    pub fn pick(&self, client_ip: Option<&str>) -> Option<Arc<NodeState>> {
        let healthy: Vec<Arc<NodeState>> =
            self.nodes.iter().filter(|n| n.is_healthy()).cloned().collect();

        if healthy.is_empty() {
            return None;
        }

        match self.strategy {
            LoadBalanceStrategy::RoundRobin => {
                let idx = self.rr_counter.fetch_add(1, Ordering::Relaxed) % healthy.len();
                Some(healthy[idx].clone())
            }
            LoadBalanceStrategy::Weighted => self.pick_weighted(&healthy),
            LoadBalanceStrategy::LeastConn => {
                healthy
                    .iter()
                    .min_by_key(|n| n.active_connections.load(Ordering::Relaxed))
                    .cloned()
            }
            LoadBalanceStrategy::IpHash => {
                let hash = simple_hash(client_ip.unwrap_or("0.0.0.0"));
                let idx = hash % healthy.len();
                Some(healthy[idx].clone())
            }
        }
    }

    /// 加权轮询选择
    fn pick_weighted(&self, healthy: &[Arc<NodeState>]) -> Option<Arc<NodeState>> {
        let total_weight: u32 = healthy.iter().map(|n| n.weight).sum();
        if total_weight == 0 {
            return healthy.first().cloned();
        }
        let counter = self.rr_counter.fetch_add(1, Ordering::Relaxed) as u32;
        let target = counter % total_weight;
        let mut cumulative = 0u32;
        for node in healthy {
            cumulative += node.weight;
            if target < cumulative {
                return Some(node.clone());
            }
        }
        healthy.last().cloned()
    }
}

/// 简单哈希函数（用于 ip_hash 策略）
fn simple_hash(s: &str) -> usize {
    let mut h: usize = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as usize);
    }
    h
}

// ─────────────────────────────────────────────
// 全局上游池注册表
// ─────────────────────────────────────────────

/// 按站点名 + 上游组名索引的上游池注册表
#[derive(Default)]
pub struct UpstreamRegistry {
    pools: RwLock<HashMap<String, Arc<UpstreamPool>>>,
}

impl UpstreamRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册上游池（key = "site_name/upstream_name"）
    pub async fn register(&self, key: String, pool: UpstreamPool) {
        let mut pools = self.pools.write().await;
        pools.insert(key, Arc::new(pool));
    }

    /// 查找上游池
    pub async fn get(&self, key: &str) -> Option<Arc<UpstreamPool>> {
        let pools = self.pools.read().await;
        pools.get(key).cloned()
    }
}

// ─────────────────────────────────────────────
// 主处理函数（xitca WebContext 版本）
// ─────────────────────────────────────────────

/// 处理反向代理请求
pub async fn handle_xitca(
    ctx: &WebContext<'_, AppState>,
    site: &SiteInfo,
    location: &LocationConfig,
) -> WebResponse {
    let upstream_name = match &location.upstream {
        Some(n) => n.clone(),
        None => return proxy_error(StatusCode::INTERNAL_SERVER_ERROR, "反向代理 location 未配置 upstream"),
    };

    // 在站点配置中找到对应上游组
    let upstream_cfg = match site.upstreams.iter().find(|u| u.name == upstream_name) {
        Some(u) => u.clone(),
        None => return proxy_error(StatusCode::INTERNAL_SERVER_ERROR, &format!("上游组 '{}' 未找到", upstream_name)),
    };

    // 构建上游池
    let pool = UpstreamPool::from_config(&upstream_cfg);

    // 提取客户端 IP 用于 ip_hash 策略
    let client_ip = Some(ctx.req().body().socket_addr().ip().to_string());

    let node = match pool.pick(client_ip.as_deref()) {
        Some(n) => n,
        None => return proxy_error(StatusCode::BAD_GATEWAY, "所有上游节点均不可用"),
    };

    // 提取请求信息
    let method  = ctx.req().method().as_str().to_string();
    let path    = ctx.req().uri().path_and_query().map(|p| p.as_str()).unwrap_or("/").to_string();
    let host    = ctx.req().headers().get("host")
                     .and_then(|v| v.to_str().ok()).unwrap_or("").to_string();

    node.active_connections.fetch_add(1, Ordering::Relaxed);
    let result = forward_request(&node.addr, &method, &path, &host, node.tls, &node.tls_sni, node.tls_insecure).await;
    node.active_connections.fetch_sub(1, Ordering::Relaxed);

    match result {
        Ok(resp) => {
            node.fail_count.store(0, Ordering::Relaxed);
            resp
        }
        Err(e) => {
            node.fail_count.fetch_add(1, Ordering::Relaxed);
            if node.fail_count.load(Ordering::Relaxed) >= 3 {
                node.mark_unhealthy();
            }
            error!("反向代理转发失败 → {}: {}", node.addr, e);
            proxy_error(StatusCode::BAD_GATEWAY, &format!("上游节点 {} 响应失败", node.addr))
        }
    }
}

// ─────────────────────────────────────────────
// 核心转发函数：HTTP / HTTPS / WS / WSS
// ─────────────────────────────────────────────

/// 向上游转发请求，自动选择 HTTP/HTTPS/WS/WSS
///
/// 协议由调用方传入的 `use_tls` 控制：
/// - HTTP  → TCP 明文 + HTTP/1.1 请求
/// - HTTPS → TCP + TLS + HTTP/1.1 请求
/// - WS    → TCP 明文 + HTTP Upgrade
/// - WSS   → TCP + TLS + HTTP Upgrade
#[allow(clippy::too_many_arguments)]
async fn forward_request(
    upstream_addr: &str,
    method: &str,
    path: &str,
    host: &str,
    use_tls: bool,
    tls_sni: &str,
    tls_insecure: bool,
) -> Result<WebResponse> {
    debug!("转发 {} {} → {} (tls={})", method, path, upstream_addr, use_tls);

    // 建立 TCP 连接（超时 10 秒）
    let tcp = tokio::time::timeout(
        Duration::from_secs(10),
        TcpStream::connect(upstream_addr),
    ).await
    .map_err(|_| anyhow::anyhow!("连接上游超时: {}", upstream_addr))??;

    // 构建统一 IO 枚举（避免 Rust trait object 多非 auto trait 限制）
    let is_ws = method.eq_ignore_ascii_case("GET"); // WS 升级请求用 GET
    if use_tls {
        let tls = tls_connect(tcp, tls_sni, tls_insecure).await?;
        send_and_recv_tls(tls, method, path, host, is_ws).await
    } else {
        send_and_recv_tcp(tcp, method, path, host, is_ws).await
    }
}

/// TCP 明文转发
async fn send_and_recv_tcp(
    io: TcpStream,
    method: &str,
    path: &str,
    host: &str,
    is_ws: bool,
) -> Result<WebResponse> {
    let (r, w) = tokio::io::split(io);
    send_and_recv_inner(r, w, method, path, host, is_ws).await
}

/// TLS 加密转发
async fn send_and_recv_tls(
    io: tokio_rustls::client::TlsStream<TcpStream>,
    method: &str,
    path: &str,
    host: &str,
    is_ws: bool,
) -> Result<WebResponse> {
    let (r, w) = tokio::io::split(io);
    send_and_recv_inner(r, w, method, path, host, is_ws).await
}

/// 通用 HTTP/1.1 请求发送和响应读取
/// 支持任意 AsyncRead + AsyncWrite 类型
async fn send_and_recv_inner<R, W>(
    reader: R,
    mut writer: W,
    method: &str,
    path: &str,
    host: &str,
    _is_ws: bool,
) -> Result<WebResponse>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    #[allow(unused_imports)]
    use tokio::io::AsyncBufReadExt as _;

    // 构造请求行和基础头
    let req = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: {host}\r\n\
         X-Forwarded-By: Sweety/0.1\r\n\
         Connection: close\r\n\
         \r\n"
    );
    writer.write_all(req.as_bytes()).await?;
    writer.flush().await?;

    // 读取响应（超时 30 秒）
    let mut buf = BufReader::new(reader);

    let mut status_line = String::new();
    tokio::time::timeout(Duration::from_secs(30), buf.read_line(&mut status_line))
        .await
        .map_err(|_| anyhow::anyhow!("等待上游响应超时"))??;

    let status_code = parse_status_code(&status_line);

    // 读取所有响应头
    let mut content_length: Option<usize> = None;
    let mut content_type = String::from("application/octet-stream");
    let mut response_headers: Vec<(String, String)> = Vec::new();
    loop {
        let mut line = String::new();
        buf.read_line(&mut line).await?;
        let trimmed = line.trim();
        if trimmed.is_empty() { break; }
        let lower = trimmed.to_lowercase();
        if lower.starts_with("content-length:") {
            content_length = trimmed[15..].trim().parse().ok();
        } else if lower.starts_with("content-type:") {
            content_type = trimmed[13..].trim().to_string();
        }
        // 保存所有响应头（用于透传给客户端）
        if let Some(colon) = trimmed.find(':') {
            response_headers.push((
                trimmed[..colon].trim().to_string(),
                trimmed[colon + 1..].trim().to_string(),
            ));
        }
    }

    // 101 Switching Protocols（WS 握手成功）：不读 body，直接返回
    if status_code == 101 {
        let mut resp = WebResponse::new(ResponseBody::empty());
        *resp.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
        return Ok(resp);
    }

    // 读取响应体
    let body = if let Some(len) = content_length {
        let mut b = vec![0u8; len];
        buf.read_exact(&mut b).await?;
        b
    } else {
        let mut b = Vec::new();
        buf.read_to_end(&mut b).await?;
        b
    };

    let http_status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::OK);
    let mut resp = WebResponse::new(ResponseBody::from(body));
    *resp.status_mut() = http_status;
    if let Ok(v) = HeaderValue::from_str(&content_type) {
        resp.headers_mut().insert(CONTENT_TYPE, v);
    }
    // 透传上游响应头（跳过已处理的 content-type/content-length）
    for (k, v) in &response_headers {
        let kl = k.to_lowercase();
        if kl == "content-type" || kl == "content-length" || kl == "transfer-encoding" {
            continue;
        }
        if let (Ok(name), Ok(val)) = (
            xitca_web::http::header::HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            resp.headers_mut().insert(name, val);
        }
    }

    Ok(resp)
}

/// 解析 HTTP 状态行中的状态码（如 "HTTP/1.1 200 OK" → 200）
fn parse_status_code(status_line: &str) -> u16 {
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(502)
}

// ─────────────────────────────────────────────
// 健康检查任务
// ─────────────────────────────────────────────

/// 主动健康检查后台任务
///
/// 对所有节点定期发送 HEAD 请求，支持 HTTP 和 HTTPS 上游
pub async fn health_check_task(pool: Arc<UpstreamPool>, check_path: String, interval_secs: u64) {
    loop {
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;

        for node in &pool.nodes {
            let addr    = node.addr.clone();
            let path    = check_path.clone();
            let use_tls = node.tls;
            let sni     = node.tls_sni.clone();
            let insecure = node.tls_insecure;

            let result = tokio::time::timeout(
                Duration::from_secs(5),
                probe_health(&addr, &path, use_tls, &sni, insecure),
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

/// 发送 HEAD 请求探活，支持 HTTP/HTTPS
async fn probe_health(addr: &str, path: &str, use_tls: bool, sni: &str, insecure: bool) -> Result<u16> {
    let tcp = TcpStream::connect(addr).await?;
    let req = format!("HEAD {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    if use_tls {
        let tls = tls_connect(tcp, sni, insecure).await?;
        let (r, mut w) = tokio::io::split(tls);
        w.write_all(req.as_bytes()).await?;
        w.flush().await?;
        let mut buf: BufReader<_> = BufReader::new(r);
        let mut line = String::new();
        buf.read_line(&mut line).await?;
        Ok(parse_status_code(&line))
    } else {
        let (r, mut w) = tokio::io::split(tcp);
        w.write_all(req.as_bytes()).await?;
        w.flush().await?;
        let mut buf: BufReader<_> = BufReader::new(r);
        let mut line = String::new();
        buf.read_line(&mut line).await?;
        Ok(parse_status_code(&line))
    }
}

/// 构造代理错误响应
fn proxy_error(status: StatusCode, _msg: &str) -> WebResponse {
    let body = crate::handler::error_page::build_default_html(status.as_u16());
    let mut resp = WebResponse::new(ResponseBody::from(body));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp
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
            nodes: nodes
                .iter()
                .map(|(addr, weight)| {
                    Arc::new(NodeState::new(&UpstreamNode {
                        addr: addr.to_string(),
                        weight: *weight,
                    }))
                })
                .collect(),
            strategy,
            rr_counter: AtomicUsize::new(0),
        }
    }

    #[test]
    fn test_round_robin() {
        let pool = make_pool(
            LoadBalanceStrategy::RoundRobin,
            &[("a:80", 1), ("b:80", 1), ("c:80", 1)],
        );
        let addrs: Vec<String> = (0..6).map(|_| pool.pick(None).unwrap().addr.clone()).collect();
        // 应轮询三个节点，每个出现两次
        assert_eq!(addrs[0], "a:80");
        assert_eq!(addrs[1], "b:80");
        assert_eq!(addrs[2], "c:80");
        assert_eq!(addrs[3], "a:80");
    }

    #[test]
    fn test_unhealthy_node_skipped() {
        let pool = make_pool(
            LoadBalanceStrategy::RoundRobin,
            &[("a:80", 1), ("b:80", 1)],
        );
        pool.nodes[0].mark_unhealthy();
        // 所有请求都应走 b
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
        let pool = make_pool(
            LoadBalanceStrategy::LeastConn,
            &[("a:80", 1), ("b:80", 1)],
        );
        pool.nodes[0].active_connections.store(5, Ordering::Relaxed);
        pool.nodes[1].active_connections.store(1, Ordering::Relaxed);
        assert_eq!(pool.pick(None).unwrap().addr, "b:80");
    }

    #[test]
    fn test_ip_hash_consistent() {
        let pool = make_pool(
            LoadBalanceStrategy::IpHash,
            &[("a:80", 1), ("b:80", 1), ("c:80", 1)],
        );
        let ip = "192.168.1.100";
        let first = pool.pick(Some(ip)).unwrap().addr.clone();
        // 相同 IP 应始终路由到同一节点
        for _ in 0..10 {
            assert_eq!(pool.pick(Some(ip)).unwrap().addr, first);
        }
    }
}
