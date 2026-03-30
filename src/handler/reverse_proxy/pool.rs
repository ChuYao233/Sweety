//! 上游连接池
//!
//! 每个上游节点（addr）维护一个 idle 连接队列，请求结束后归还连接复用。
//! 支持 TCP 和 TLS 两种连接类型，通过枚举统一管理。
//!
//! 设计原则：
//! - 取连接：先从 idle 队列取，取不到则新建
//! - 归还连接：响应头中无 `Connection: close` 则归还，否则丢弃
//! - 空闲超时：idle 连接超过 idle_timeout 秒自动丢弃（惰性清理）
//! - 最大 idle 数：超过上限时丢弃多余连接（防止资源泄漏）

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

/// 连接池全局实例，按节点 key（addr:port）索引
#[derive(Clone)]
pub struct ConnPool {
    inner: Arc<DashMap<String, NodePool>>,
    /// 每节点最大 idle 连接数
    max_idle: usize,
    /// idle 连接最大空闲时间（秒）
    idle_timeout: Duration,
}

impl ConnPool {
    pub fn new(max_idle: usize, idle_timeout_secs: u64) -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            max_idle,
            idle_timeout: Duration::from_secs(idle_timeout_secs),
        }
    }

    /// 从连接池取一个 idle 连接；若无则返回 None（调用方新建）
    pub fn acquire(&self, key: &str) -> Option<PooledConn> {
        let mut entry = self.inner.get_mut(key)?;
        let pool = entry.value_mut();
        let now = Instant::now();
        // 惰性清理过期连接
        pool.idle.retain(|c| now.duration_since(c.returned_at) < self.idle_timeout);
        pool.idle.pop_front().map(|c| c.conn)
    }

    /// 归还连接到池
    pub fn release(&self, key: String, conn: PooledConn) {
        let mut entry = self.inner.entry(key).or_default();
        let pool = entry.value_mut();
        if pool.idle.len() >= self.max_idle {
            return; // 超过上限，丢弃
        }
        pool.idle.push_back(IdleConn {
            conn,
            returned_at: Instant::now(),
        });
    }
}

/// 单节点连接池
#[derive(Default)]
struct NodePool {
    idle: VecDeque<IdleConn>,
}

struct IdleConn {
    conn: PooledConn,
    returned_at: Instant,
}

/// 池化连接（TCP 或 TLS 统一枚举）
pub enum PooledConn {
    Tcp(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl PooledConn {
    /// 连接 key（用于池索引）
    pub fn key(addr: &str, tls: bool) -> String {
        format!("{}:{}", if tls { "tls" } else { "tcp" }, addr)
    }
}

/// TcpStream 和 TlsStream 在实际使用中都是 Unpin 安全的
impl Unpin for PooledConn {}

/// 为 PooledConn 实现 AsyncRead + AsyncWrite，统一读写接口
impl AsyncRead for PooledConn {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            PooledConn::Tcp(s) => std::pin::Pin::new(s).poll_read(cx, buf),
            PooledConn::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for PooledConn {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            PooledConn::Tcp(s) => std::pin::Pin::new(s).poll_write(cx, buf),
            PooledConn::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            PooledConn::Tcp(s) => std::pin::Pin::new(s).poll_flush(cx),
            PooledConn::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            PooledConn::Tcp(s) => std::pin::Pin::new(s).poll_shutdown(cx),
            PooledConn::Tls(s) => std::pin::Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}
