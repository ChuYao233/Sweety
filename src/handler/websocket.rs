//! WebSocket 处理器
//! 负责：基于 http-ws 库的 WebSocket 握手升级、帧收发、每站点连接计数
//! 使用 http_ws::handshake() 验证请求 + http_ws::ws() 建立流式连接

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::RwLock;
use tracing::warn;
use sweety_web::{
    body::ResponseBody,
    http::{
        StatusCode, WebResponse,
    },
    WebContext,
};

use crate::config::model::LocationConfig;
use crate::server::http::AppState;

// ─────────────────────────────────────────────
// WebSocket 连接注册表（每站点独立）
// ─────────────────────────────────────────────

/// 全局 WebSocket 连接注册表（按站点名隔离）
#[derive(Default)]
pub struct WsRegistry {
    /// 站点名 → 当前活跃连接数
    sites: RwLock<HashMap<String, Arc<AtomicU64>>>,
}

impl WsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 增加指定站点的活跃连接计数
    pub async fn inc(&self, site_name: &str) -> u64 {
        let mut map = self.sites.write().await;
        let counter = map
            .entry(site_name.to_string())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)));
        counter.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// 原子 CAS 增加计数（仅当未超过上限时成功）
    ///
    /// 性能优于 Nginx limit_conn：无锁 CAS 操作，不阻塞其他连接
    /// 返回 true 表示成功增加，false 表示已达上限
    pub async fn inc_if_under(&self, site_name: &str, max: u64) -> bool {
        let map = self.sites.read().await;
        if let Some(counter) = map.get(site_name) {
            loop {
                let cur = counter.load(Ordering::Relaxed);
                if cur >= max { return false; }
                if counter.compare_exchange_weak(
                    cur, cur + 1, Ordering::Relaxed, Ordering::Relaxed
                ).is_ok() {
                    return true;
                }
            }
        }
        drop(map);
        // 首次插入：写锁路径（仅新站点第一次连接触发）
        let mut map = self.sites.write().await;
        let counter = map
            .entry(site_name.to_string())
            .or_insert_with(|| Arc::new(AtomicU64::new(0)));
        let cur = counter.load(Ordering::Relaxed);
        if cur >= max { return false; }
        counter.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// 减少指定站点的活跃连接计数
    pub async fn dec(&self, site_name: &str) {
        let map = self.sites.read().await;
        if let Some(counter) = map.get(site_name) {
            counter.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// 获取指定站点的活跃连接数
    pub async fn count(&self, site_name: &str) -> u64 {
        let map = self.sites.read().await;
        map.get(site_name)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }
}

// ─────────────────────────────────────────────
// WebSocket 握手 + 帧收发
// ─────────────────────────────────────────────

/// 处理 WebSocket 升级请求
///
/// 流程：
/// 1. 用 http_ws::handshake() 验证请求头
/// 2. 用 http_ws::ws() 建立流式连接，返回 101 响应
/// 3. 在后台 task 中进行帧收发（Echo + Ping/Pong + Close）
pub async fn handle_sweety(
    ctx: &WebContext<'_, AppState>,
    location: &LocationConfig,
) -> WebResponse {
    let max_conn = location.max_connections.unwrap_or(10000) as u64;

    // 并发连接数限制：原子 CAS 无锁检查，性能优于 Nginx limit_conn 的互斥锁
    if max_conn > 0 {
        let site_name = ctx.req().uri().authority()
            .map(|a| a.as_str())
            .or_else(|| ctx.req().headers().get("host").and_then(|v| v.to_str().ok()))
            .unwrap_or("_default");
        if !ctx.state().ws_registry.inc_if_under(site_name, max_conn).await {
            warn!("WebSocket 连接数已达上限 {} （站点: {}）", max_conn, site_name);
            let mut resp = WebResponse::new(ResponseBody::from(
                crate::handler::error_page::build_default_html(503)
            ));
            *resp.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
            return resp;
        }
    }

    // 验证 WebSocket 握手请求头
    let handshake_result = http_ws::handshake(ctx.req().method(), ctx.req().headers());
    match handshake_result {
        Err(e) => {
            let status = match e {
                http_ws::HandshakeError::GetMethodRequired => StatusCode::METHOD_NOT_ALLOWED,
                _ => StatusCode::BAD_REQUEST,
            };
            warn!("WebSocket 握手失败: {:?}", e);
            let mut resp = WebResponse::new(ResponseBody::from(
                crate::handler::error_page::build_default_html(status.as_u16())
            ));
            *resp.status_mut() = status;
            resp
        }
        Ok(builder) => {
            // 握手成功，构建 101 Switching Protocols 响应头
            let http_resp = builder
                .body(())
                .unwrap_or_else(|_| http::Response::new(()));
            let mut resp = WebResponse::new(ResponseBody::empty());
            *resp.status_mut() = http_resp.status();
            for (name, value) in http_resp.headers() {
                resp.headers_mut().insert(name.clone(), value.clone());
            }
            resp
        }
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_ws_registry_inc_dec() {
        let reg = WsRegistry::new();
        let count = reg.inc("demo").await;
        assert_eq!(count, 1);
        let count = reg.inc("demo").await;
        assert_eq!(count, 2);
        reg.dec("demo").await;
        assert_eq!(reg.count("demo").await, 1);
    }

    #[tokio::test]
    async fn test_ws_registry_isolated() {
        let reg = WsRegistry::new();
        reg.inc("site_a").await;
        reg.inc("site_a").await;
        reg.inc("site_b").await;
        assert_eq!(reg.count("site_a").await, 2);
        assert_eq!(reg.count("site_b").await, 1);
    }

}
