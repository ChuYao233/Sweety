//! FastCGI 连接池
//!
//! 每个 Unix socket / TCP 地址维护一个 idle 连接队列（`VecDeque`），
//! 原理与 `reverse_proxy/pool.rs` 完全相同：
//! - `acquire`：先取 idle 连接；无 idle 则新建
//! - `release`：归还可复用的连接
//! - 连接建立失败不 panic，返回 `Err`
//!
//! 生命周期：随 `AppState` Arc 共享，进程级单例。

use std::collections::VecDeque;
use dashmap::DashMap;

/// FastCGI 连接池（进程级，线程安全）
///
/// 用 DashMap 替代 Mutex<HashMap>：DashMap 内部分片锁，
/// 高并发下几乎无竞争，避免在 tokio async worker 内阻塞。
pub struct FcgiPool {
    /// key = socket 地址（Unix socket 路径 或 "host:port"）
    inner: DashMap<String, VecDeque<FcgiConn>>,
    /// 每个地址最多保留的 idle 连接数
    max_idle: usize,
    /// 连接超时（秒）
    pub connect_timeout_secs: u64,
    /// 读超时（秒）
    pub read_timeout_secs: u64,
}

/// 一个已建立的 FastCGI 流（TCP 或 Unix socket）
pub enum FcgiConn {
    Tcp(tokio::net::TcpStream),
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
}

impl FcgiPool {
    /// 创建新连接池
    pub fn new(max_idle: usize, connect_timeout_secs: u64, read_timeout_secs: u64) -> Self {
        Self {
            inner: DashMap::new(),
            max_idle,
            connect_timeout_secs,
            read_timeout_secs,
        }
    }

    /// 从池中取出 idle 连接，没有则新建
    /// DashMap 分片锁：不同地址的请求并行无竞争。
    pub async fn acquire(&self, addr: &str, is_unix: bool) -> anyhow::Result<FcgiConn> {
        // 先尝试取 idle（分片锁，不阻塞 worker）
        if let Some(mut queue) = self.inner.get_mut(addr) {
            if let Some(conn) = queue.pop_front() {
                return Ok(conn);
            }
        }
        // 新建连接（异步，锁已释放）
        let timeout = std::time::Duration::from_secs(self.connect_timeout_secs);
        let conn = tokio::time::timeout(timeout, new_conn(addr, is_unix)).await
            .map_err(|_| anyhow::anyhow!("FastCGI 连接超时 {}", addr))??;
        Ok(conn)
    }

    /// 归还连接到 idle 队列（超出 max_idle 则 drop）
    pub fn release(&self, addr: &str, conn: FcgiConn) {
        let mut queue = self.inner.entry(addr.to_string()).or_insert_with(VecDeque::new);
        if queue.len() < self.max_idle {
            queue.push_back(conn);
        }
        // 超出 max_idle：conn 在这里 drop，自动关闭 fd
    }

    /// 清空指定地址的所有 idle 连接（地址变更时调用）
    pub fn evict(&self, addr: &str) {
        self.inner.remove(addr);
    }
}

/// 新建一个 FastCGI 连接
async fn new_conn(addr: &str, is_unix: bool) -> anyhow::Result<FcgiConn> {
    if is_unix {
        #[cfg(unix)]
        {
            let stream = tokio::net::UnixStream::connect(addr).await?;
            return Ok(FcgiConn::Unix(stream));
        }
        #[cfg(not(unix))]
        {
            // Windows fallback: TCP
            let _ = is_unix;
            let stream = tokio::net::TcpStream::connect(addr).await?;
            return Ok(FcgiConn::Tcp(stream));
        }
    }
    let stream = tokio::net::TcpStream::connect(addr).await?;
    // TCP_NODELAY 减少延迟（与 Nginx proxy_socket_keepalive 类似）
    let _ = stream.set_nodelay(true);
    Ok(FcgiConn::Tcp(stream))
}
