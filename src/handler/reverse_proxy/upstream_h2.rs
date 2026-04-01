//! HTTP/2 上游连接池
//! h2c（明文）或 h2 over TLS，单连接多路复用 stream

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow};
use bytes::Bytes;
use dashmap::DashMap;
use h2::client::{self, SendRequest};
use http::Request;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use super::tls_client::tls_connect;

/// 单条 H2 连接句柄
struct H2Conn {
    sender: SendRequest<Bytes>,
}

/// 单节点 H2 连接池
pub struct H2NodePool {
    addr: String,
    use_tls: bool,
    tls_sni: String,
    tls_insecure: bool,
    max_conns: usize,
    connect_timeout: Duration,
    conns: Mutex<Vec<H2Conn>>,
}

impl H2NodePool {
    pub fn new(
        addr: &str,
        use_tls: bool,
        tls_sni: &str,
        tls_insecure: bool,
        max_conns: usize,
        connect_timeout_secs: u64,
    ) -> Self {
        Self {
            addr: addr.to_string(),
            use_tls,
            tls_sni: tls_sni.to_string(),
            tls_insecure,
            max_conns: max_conns.max(1),
            connect_timeout: Duration::from_secs(if connect_timeout_secs > 0 { connect_timeout_secs } else { 10 }),
            conns: Mutex::new(Vec::new()),
        }
    }

    /// 获取可用 SendRequest：优先复用现有连接，不足时新建
    async fn get_sender(&self) -> Result<SendRequest<Bytes>> {
        let mut guard = self.conns.lock().await;
        // 清理已关闭的连接
        guard.retain(|c| !c.sender.is_closed());
        // 尝试复用：clone 出 SendRequest（共享同一条 TCP），等待流量控制窗口
        for conn in guard.iter_mut() {
            match conn.sender.clone().ready().await {
                Ok(s) => return Ok(s),
                Err(_) => continue, // 连接已失效，继续找下一条
            }
        }
        // 池未满：新建连接
        if guard.len() < self.max_conns {
            let sender = self.new_conn().await?;
            let s = sender.clone();
            guard.push(H2Conn { sender });
            return s.ready().await.map_err(|e| anyhow!("h2 ready: {e}"));
        }
        // 池满且全部忙：等第一条连接的流量控制窗口
        if let Some(conn) = guard.first_mut() {
            return conn.sender.clone().ready().await.map_err(|e| anyhow!("h2 ready: {e}"));
        }
        Err(anyhow!("h2 连接池为空"))
    }

    /// 新建一条 H2 连接
    async fn new_conn(&self) -> Result<SendRequest<Bytes>> {
        let tcp = tokio::time::timeout(
            self.connect_timeout,
            TcpStream::connect(&self.addr),
        ).await
        .map_err(|_| anyhow!("h2 connect timeout: {}", self.addr))?
        .map_err(|e| anyhow!("h2 connect: {e}"))?;
        let _ = tcp.set_nodelay(true);

        if self.use_tls {
            let tls = tls_connect(tcp, &self.tls_sni, self.tls_insecure).await?;
            let (sender, conn) = client::Builder::new()
                .initial_window_size(1 << 20)   // 1MB 流量控制窗口
                .initial_connection_window_size(1 << 21) // 2MB 连接级窗口
                .handshake(tls).await
                .map_err(|e| anyhow!("h2 tls handshake: {e}"))?;
            tokio::spawn(async move { let _ = conn.await; });
            Ok(sender)
        } else {
            let (sender, conn) = client::Builder::new()
                .initial_window_size(1 << 20)
                .initial_connection_window_size(1 << 21)
                .handshake(tcp).await
                .map_err(|e| anyhow!("h2c handshake: {e}"))?;
            tokio::spawn(async move { let _ = conn.await; });
            Ok(sender)
        }
    }

    /// 向上游发送请求，返回响应头和 body stream
    pub async fn send(
        &self,
        req: Request<()>,
        body: Option<Bytes>,
    ) -> Result<(http::response::Parts, h2::RecvStream)> {
        let mut sender = self.get_sender().await?;
        let end_stream = body.is_none() || body.as_ref().map(|b| b.is_empty()).unwrap_or(true);
        let (resp_fut, mut send_stream) = sender
            .send_request(req, end_stream)
            .map_err(|e| anyhow!("h2 send_request: {e}"))?;

        // 发送请求体
        if let Some(b) = body {
            if !b.is_empty() {
                send_stream.send_data(b, true)
                    .map_err(|e| anyhow!("h2 send_data: {e}"))?;
            }
        }

        let resp = resp_fut.await.map_err(|e| anyhow!("h2 response: {e}"))?;
        let (parts, recv) = resp.into_parts();
        Ok((parts, recv))
    }
}

/// 全局 H2 上游连接池注册表（addr → pool）
#[derive(Default)]
pub struct H2UpstreamPools {
    inner: DashMap<String, Arc<H2NodePool>>,
}

impl H2UpstreamPools {
    pub fn new() -> Self { Self::default() }

    pub fn get_or_create(
        &self,
        addr: &str,
        use_tls: bool,
        tls_sni: &str,
        tls_insecure: bool,
        max_conns: usize,
        connect_timeout_secs: u64,
    ) -> Arc<H2NodePool> {
        let key = format!("{}:{}", if use_tls {"tls"} else {"plain"}, addr);
        self.inner.entry(key).or_insert_with(|| {
            Arc::new(H2NodePool::new(addr, use_tls, tls_sni, tls_insecure, max_conns, connect_timeout_secs))
        }).clone()
    }
}
