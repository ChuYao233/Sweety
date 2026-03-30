//! Prometheus 指标导出模块
//! 负责：将内部计数器格式化为 Prometheus text exposition format
//! 暴露在 /metrics 接口（挂载在 admin_listen 端口上）

use crate::middleware::metrics::MetricsSnapshot;
use crate::monitor::analyzer::AnalysisReport;

/// 将指标快照格式化为 Prometheus text 格式
pub fn format_metrics(snapshot: &MetricsSnapshot, report: Option<&AnalysisReport>) -> String {
    let mut output = String::with_capacity(2048);

    // ── 基础请求计数器 ──────────────────────────────────
    output.push_str("# HELP sweety_requests_total 累计处理请求总数\n");
    output.push_str("# TYPE sweety_requests_total counter\n");
    output.push_str(&format!("sweety_requests_total {}\n\n", snapshot.total_requests));

    output.push_str("# HELP sweety_active_requests 当前并发请求数\n");
    output.push_str("# TYPE sweety_active_requests gauge\n");
    output.push_str(&format!("sweety_active_requests {}\n\n", snapshot.active_requests));

    // ── 错误计数器 ──────────────────────────────────────
    output.push_str("# HELP sweety_errors_4xx_total 累计 4xx 错误数\n");
    output.push_str("# TYPE sweety_errors_4xx_total counter\n");
    output.push_str(&format!("sweety_errors_4xx_total {}\n\n", snapshot.total_errors_4xx));

    output.push_str("# HELP sweety_errors_5xx_total 累计 5xx 错误数\n");
    output.push_str("# TYPE sweety_errors_5xx_total counter\n");
    output.push_str(&format!("sweety_errors_5xx_total {}\n\n", snapshot.total_errors_5xx));

    // ── 流量统计 ────────────────────────────────────────
    output.push_str("# HELP sweety_bytes_sent_total 累计响应字节数\n");
    output.push_str("# TYPE sweety_bytes_sent_total counter\n");
    output.push_str(&format!("sweety_bytes_sent_total {}\n\n", snapshot.total_bytes_sent));

    // ── WebSocket ────────────────────────────────────────
    output.push_str("# HELP sweety_websocket_connections 当前活跃 WebSocket 连接数\n");
    output.push_str("# TYPE sweety_websocket_connections gauge\n");
    output.push_str(&format!(
        "sweety_websocket_connections {}\n\n",
        snapshot.active_ws_connections
    ));

    // ── 分析报告（可选）────────────────────────────────
    if let Some(report) = report {
        output.push_str("# HELP sweety_avg_response_ms 近期平均响应时间（毫秒）\n");
        output.push_str("# TYPE sweety_avg_response_ms gauge\n");
        output.push_str(&format!("sweety_avg_response_ms {:.2}\n\n", report.avg_duration_ms));

        output.push_str("# HELP sweety_error_rate_5xx 近期 5xx 错误率\n");
        output.push_str("# TYPE sweety_error_rate_5xx gauge\n");
        output.push_str(&format!("sweety_error_rate_5xx {:.6}\n\n", report.error_rate_5xx));

        // 状态码分布（以 label 区分）
        output.push_str("# HELP sweety_status_total 近期各状态码请求数\n");
        output.push_str("# TYPE sweety_status_total counter\n");
        let mut status_vec: Vec<(u16, u64)> = report.status_distribution.iter()
            .map(|(&k, &v)| (k, v))
            .collect();
        status_vec.sort_by_key(|(k, _)| *k);
        for (code, count) in &status_vec {
            output.push_str(&format!(
                "sweety_status_total{{code=\"{}\"}} {}\n",
                code, count
            ));
        }
        output.push('\n');
    }

    output
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::metrics::MetricsSnapshot;

    fn sample_snapshot() -> MetricsSnapshot {
        MetricsSnapshot {
            total_requests: 1000,
            total_errors_5xx: 5,
            total_errors_4xx: 20,
            total_bytes_sent: 1024 * 1024,
            active_ws_connections: 3,
            active_requests: 10,
        }
    }

    #[test]
    fn test_format_contains_total_requests() {
        let snap = sample_snapshot();
        let output = format_metrics(&snap, None);
        assert!(output.contains("sweety_requests_total 1000"));
    }

    #[test]
    fn test_format_contains_all_metrics() {
        let snap = sample_snapshot();
        let output = format_metrics(&snap, None);
        assert!(output.contains("sweety_errors_5xx_total 5"));
        assert!(output.contains("sweety_errors_4xx_total 20"));
        assert!(output.contains("sweety_bytes_sent_total"));
        assert!(output.contains("sweety_websocket_connections 3"));
    }

    #[test]
    fn test_format_with_report() {
        use crate::monitor::analyzer::{AnalysisReport, SlowRequest};
        use std::collections::HashMap;

        let mut status_dist = HashMap::new();
        status_dist.insert(200u16, 900u64);
        status_dist.insert(500u16, 5u64);

        let report = AnalysisReport {
            total_requests: 905,
            avg_duration_ms: 42.5,
            slow_requests: vec![],
            hot_paths: vec![("/api".into(), 500)],
            status_distribution: status_dist,
            total_bytes: 1024,
            error_rate_5xx: 0.005,
        };

        let snap = sample_snapshot();
        let output = format_metrics(&snap, Some(&report));
        assert!(output.contains("sweety_avg_response_ms 42.50"));
        assert!(output.contains("sweety_error_rate_5xx"));
        assert!(output.contains("code=\"200\""));
        assert!(output.contains("code=\"500\""));
    }

    #[test]
    fn test_format_is_valid_prometheus_text() {
        let snap = sample_snapshot();
        let output = format_metrics(&snap, None);
        // 每个 HELP 行后面应有 TYPE 行
        let help_count = output.matches("# HELP").count();
        let type_count = output.matches("# TYPE").count();
        assert_eq!(help_count, type_count);
    }
}
