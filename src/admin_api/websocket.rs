//! 管理 WebSocket API 模块
//! 负责：实时统计数据推送、接收管理指令
//! 连接地址：ws://<admin_listen>/api/v1/stats/stream

use std::sync::Arc;
use std::time::Duration;

use tracing::info;

use crate::middleware::metrics::GlobalMetrics;

/// WebSocket 推送消息类型
#[derive(Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PushMessage {
    /// 实时统计快照
    Stats(crate::middleware::metrics::MetricsSnapshot),
    /// 服务器事件通知
    Event { message: String },
    /// 心跳（保活）
    Ping,
}

/// 启动管理 WebSocket 实时推送服务
///
/// 当前版本：轮询模式，每秒推送一次统计快照。
/// 后续版本（v0.5）：实现完整 WebSocket 握手 + 帧收发 + 指令接收。
///
/// 完整实现步骤（规划）：
/// 1. 识别 HTTP Upgrade: websocket 请求
/// 2. 完成 WebSocket 握手（Sec-WebSocket-Accept 计算）
/// 3. 进入帧收发循环
/// 4. 每秒发送 Text 帧（stats JSON）
/// 5. 接收管理指令帧（如 reload_config / update_ratelimit）
pub async fn start_stats_stream(metrics: Arc<GlobalMetrics>, interval_secs: u64) {
    // 当前版本：仅用 tracing 输出统计，供开发阶段验证指标收集正确性
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
    loop {
        interval.tick().await;
        let snap = metrics.snapshot();
        let msg = PushMessage::Stats(snap);
        if let Ok(json) = serde_json::to_string(&msg) {
            info!(target: "admin_ws", "实时统计: {}", json);
        }
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::metrics::GlobalMetrics;

    #[test]
    fn test_push_message_serialization() {
        let metrics = GlobalMetrics::new();
        metrics.inc_requests();
        let snap = metrics.snapshot();
        let msg = PushMessage::Stats(snap);
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"stats\""));
        assert!(json.contains("total_requests"));
    }

    #[test]
    fn test_event_message() {
        let msg = PushMessage::Event {
            message: "配置已热重载".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"event\""));
        assert!(json.contains("配置已热重载"));
    }

    #[test]
    fn test_ping_message() {
        let msg = PushMessage::Ping;
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"ping\""));
    }
}
