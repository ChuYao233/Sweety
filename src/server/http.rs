//! HTTP 服务器核心模块
//! 负责：Xitca-Web 应用构建、多端口监听绑定、将请求交给 dispatcher 层处理

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::RwLock;
use tracing::info;

use crate::config::model::AppConfig;
use crate::dispatcher::vhost::VHostRegistry;
use crate::middleware::metrics::GlobalMetrics;

/// 服务器共享状态，在所有请求处理 task 之间共享
#[derive(Clone)]
pub struct AppState {
    /// 站点路由注册表（支持热重载替换）
    pub vhost_registry: Arc<RwLock<VHostRegistry>>,
    /// 全局指标计数器
    pub metrics: Arc<GlobalMetrics>,
}

/// 启动 HTTP 服务器主入口
///
/// 根据配置绑定所有端口，构建路由树，进入异步监听循环
pub async fn run(cfg: AppConfig) -> Result<()> {
    // 构建虚拟主机注册表
    let registry = VHostRegistry::from_config(&cfg.sites);
    let state = AppState {
        vhost_registry: Arc::new(RwLock::new(registry)),
        metrics: Arc::new(GlobalMetrics::new()),
    };

    // 收集所有需要监听的端口（去重）
    let mut http_ports: std::collections::HashSet<u16> = std::collections::HashSet::new();
    for site in &cfg.sites {
        for port in &site.listen {
            http_ports.insert(*port);
        }
    }
    if http_ports.is_empty() {
        http_ports.insert(80);
    }

    // 启动热重载（如果配置文件路径存在则监听）
    // 热重载任务在后台独立运行，通过 watch channel 通知主循环
    let state_for_reload = state.clone();
    let cfg_arc = Arc::new(cfg);
    let cfg_for_reload = cfg_arc.clone();

    // 启动配置热重载后台任务（后续版本完整接入，此处预留扩展点）
    tokio::spawn(async move {
        hot_reload_task(state_for_reload, cfg_for_reload).await;
    });

    // 绑定并监听所有 HTTP 端口
    let mut listeners = Vec::new();
    for port in &http_ports {
        let addr: SocketAddr = format!("0.0.0.0:{}", port).parse()?;
        let listener = tokio::net::TcpListener::bind(addr).await?;
        info!("HTTP 监听端口: {}", addr);
        listeners.push(listener);
    }

    // 为每个端口启动独立 accept 循环
    let mut handles = Vec::new();
    for listener in listeners {
        let state = state.clone();
        let handle = tokio::spawn(async move {
            accept_loop(listener, state).await;
        });
        handles.push(handle);
    }

    // 等待所有监听任务（正常情况下永不退出）
    for handle in handles {
        let _ = handle.await;
    }

    Ok(())
}

/// 单端口 TCP accept 循环
async fn accept_loop(listener: tokio::net::TcpListener, state: AppState) {
    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    handle_connection(stream, peer_addr, state).await;
                });
            }
            Err(e) => {
                tracing::error!("accept 连接失败: {}", e);
            }
        }
    }
}

/// 处理单个 TCP 连接
///
/// 当前实现：解析 HTTP/1.1 请求并交给 dispatcher 处理。
/// 后续版本：在此处接入 Xitca-Web 完整应用树，支持 HTTP/2 协议协商。
async fn handle_connection(
    stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    state: AppState,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (reader, mut writer) = tokio::io::split(stream);
    let mut buf_reader = BufReader::new(reader);

    // 简单读取请求行（HTTP/1.1 最小实现，后续替换为 xitca-http 完整解析）
    let mut request_line = String::new();
    if buf_reader.read_line(&mut request_line).await.is_err() {
        return;
    }

    // 解析方法、路径、Host header
    let parts: Vec<&str> = request_line.trim().splitn(3, ' ').collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0].to_string();
    let path = parts[1].to_string();

    // 读取请求头，提取 Host
    let mut host = String::new();
    loop {
        let mut line = String::new();
        if buf_reader.read_line(&mut line).await.is_err() {
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break; // 空行：请求头结束
        }
        if trimmed.to_lowercase().starts_with("host:") {
            host = trimmed[5..].trim().to_string();
        }
    }

    // 更新连接计数
    state.metrics.inc_requests();

    // 通过 dispatcher 分发请求
    let registry = state.vhost_registry.read().await;
    let response = crate::dispatcher::dispatch(&registry, &host, &method, &path, peer_addr).await;

    // 写回 HTTP/1.1 响应
    let response_bytes = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        response.status_code,
        response.status_text,
        response.body.len(),
        response.body
    );

    let _ = writer.write_all(response_bytes.as_bytes()).await;
}

/// 热重载后台任务（预留扩展点）
///
/// 当配置文件变更时，重新构建 VHostRegistry 并原子替换
async fn hot_reload_task(state: AppState, _cfg: Arc<AppConfig>) {
    // 后续版本：通过 watch::Receiver 接收新配置，更新 vhost_registry
    // 当前版本：空实现，仅占位
    tokio::time::sleep(tokio::time::Duration::from_secs(u64::MAX)).await;
    drop(state);
}
