//! ServerBuilder：Sweety 的 HTTP 服务构建入口
//!
//! 业务层通过 ServerBuilder 配置并启动 HTTP 服务，
//! 内部细节（xitca-web 类型）完全不暴露给外部。

use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context, Result};
use tokio::sync::watch;
use tracing::info;

use xitca_io::net::QuicConfig;
use xitca_web::{
    App,
    handler::handler_service,
    http::{WebResponse, header::CONTENT_TYPE},
    WebContext,
};

/// HTTP 服务配置
#[derive(Clone)]
pub struct ServerConfig {
    /// HTTP/1+2 监听地址列表
    pub http_addrs: Vec<SocketAddr>,
    /// HTTPS/2 + HTTP/3 监听地址列表（附带 TLS 配置）
    pub https_addrs: Vec<(SocketAddr, Arc<rustls::ServerConfig>)>,
    /// HTTP/3 QUIC 监听地址列表
    pub h3_addrs: Vec<(SocketAddr, QuicConfig)>,
    /// Worker 线程数（0 = 自动使用 CPU 核数）
    pub workers: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            http_addrs: vec![],
            https_addrs: vec![],
            h3_addrs: vec![],
            workers: 0,
        }
    }
}

/// HTTP 服务构建器
///
/// # 示例
/// ```rust,no_run
/// use sweety_http::ServerBuilder;
///
/// let builder = ServerBuilder::new()
///     .http("0.0.0.0:80".parse().unwrap())
///     .workers(4);
/// ```
pub struct ServerBuilder {
    config: ServerConfig,
}

impl ServerBuilder {
    pub fn new() -> Self {
        Self {
            config: ServerConfig::default(),
        }
    }

    /// 添加 HTTP 监听地址
    pub fn http(mut self, addr: SocketAddr) -> Self {
        self.config.http_addrs.push(addr);
        self
    }

    /// 添加 HTTPS/HTTP2 监听地址（需提供 rustls ServerConfig）
    pub fn https(mut self, addr: SocketAddr, tls: Arc<rustls::ServerConfig>) -> Self {
        self.config.https_addrs.push((addr, tls));
        self
    }

    /// 添加 HTTP/3 QUIC 监听地址
    pub fn h3(mut self, addr: SocketAddr, quic: QuicConfig) -> Self {
        self.config.h3_addrs.push((addr, quic));
        self
    }

    /// 设置 worker 线程数（0 = CPU 核数）
    pub fn workers(mut self, n: usize) -> Self {
        self.config.workers = n;
        self
    }

    /// 构建并返回可运行的服务配置，供 XitcaEngine 使用
    pub fn build(self) -> ServerConfig {
        self.config
    }
}

impl Default for ServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}
