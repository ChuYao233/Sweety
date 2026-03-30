//! 全局指标统计模块
//! 负责：原子计数器管理（请求量、5xx 数、带宽、活跃 WebSocket 连接数）

use std::sync::atomic::{AtomicU64, Ordering};

/// 全局指标计数器（线程安全，无锁）
#[derive(Debug, Default)]
pub struct GlobalMetrics {
    /// 总请求数
    pub total_requests: AtomicU64,
    /// 总 5xx 错误数
    pub total_errors_5xx: AtomicU64,
    /// 总 4xx 错误数
    pub total_errors_4xx: AtomicU64,
    /// 总发送字节数
    pub total_bytes_sent: AtomicU64,
    /// 活跃 WebSocket 连接数
    pub active_ws_connections: AtomicU64,
    /// 当前并发请求数
    pub active_requests: AtomicU64,
}

impl GlobalMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// 请求开始时调用
    pub fn inc_requests(&self) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);
        self.active_requests.fetch_add(1, Ordering::Relaxed);
    }

    /// 请求结束时调用
    pub fn dec_active(&self) {
        self.active_requests.fetch_sub(1, Ordering::Relaxed);
    }

    /// 记录响应状态码
    pub fn record_status(&self, status: u16) {
        if status >= 500 {
            self.total_errors_5xx.fetch_add(1, Ordering::Relaxed);
        } else if status >= 400 {
            self.total_errors_4xx.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 记录发送字节数
    pub fn record_bytes_sent(&self, bytes: u64) {
        self.total_bytes_sent.fetch_add(bytes, Ordering::Relaxed);
    }

    /// WebSocket 连接建立
    pub fn ws_connected(&self) {
        self.active_ws_connections.fetch_add(1, Ordering::Relaxed);
    }

    /// WebSocket 连接断开
    pub fn ws_disconnected(&self) {
        self.active_ws_connections.fetch_sub(1, Ordering::Relaxed);
    }

    /// 快照当前所有指标
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            total_requests:      self.total_requests.load(Ordering::Relaxed),
            total_errors_5xx:    self.total_errors_5xx.load(Ordering::Relaxed),
            total_errors_4xx:    self.total_errors_4xx.load(Ordering::Relaxed),
            total_bytes_sent:    self.total_bytes_sent.load(Ordering::Relaxed),
            active_ws_connections: self.active_ws_connections.load(Ordering::Relaxed),
            active_requests:     self.active_requests.load(Ordering::Relaxed),
        }
    }
}

/// 指标快照（用于 API 返回和 Prometheus 导出）
#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricsSnapshot {
    pub total_requests: u64,
    pub total_errors_5xx: u64,
    pub total_errors_4xx: u64,
    pub total_bytes_sent: u64,
    pub active_ws_connections: u64,
    pub active_requests: u64,
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_counting() {
        let m = GlobalMetrics::new();
        m.inc_requests();
        m.inc_requests();
        let snap = m.snapshot();
        assert_eq!(snap.total_requests, 2);
        assert_eq!(snap.active_requests, 2);

        m.dec_active();
        assert_eq!(m.snapshot().active_requests, 1);
    }

    #[test]
    fn test_status_recording() {
        let m = GlobalMetrics::new();
        m.record_status(200);
        m.record_status(404);
        m.record_status(500);
        m.record_status(503);
        let snap = m.snapshot();
        assert_eq!(snap.total_errors_4xx, 1);
        assert_eq!(snap.total_errors_5xx, 2);
    }

    #[test]
    fn test_ws_tracking() {
        let m = GlobalMetrics::new();
        m.ws_connected();
        m.ws_connected();
        m.ws_disconnected();
        assert_eq!(m.snapshot().active_ws_connections, 1);
    }

    #[test]
    fn test_bytes_sent() {
        let m = GlobalMetrics::new();
        m.record_bytes_sent(1024);
        m.record_bytes_sent(2048);
        assert_eq!(m.snapshot().total_bytes_sent, 3072);
    }
}
