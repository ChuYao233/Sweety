//! 流量分析器
//! 负责：慢请求 TopN、热点路径 TopN、错误状态码分布、带宽峰值统计

use std::collections::HashMap;
use super::collector::RequestRecord;

/// 分析报告
#[derive(Debug, Clone)]
pub struct AnalysisReport {
    /// 总请求数
    pub total_requests: usize,
    /// 平均响应时间（毫秒）
    pub avg_duration_ms: f64,
    /// 最慢请求 TopN（按耗时降序）
    pub slow_requests: Vec<SlowRequest>,
    /// 热点路径 TopN（按请求次数降序）
    pub hot_paths: Vec<(String, u64)>,
    /// 状态码分布（状态码 → 次数）
    pub status_distribution: HashMap<u16, u64>,
    /// 总发送字节数
    pub total_bytes: u64,
    /// 5xx 错误率（0.0 ~ 1.0）
    pub error_rate_5xx: f64,
}

/// 慢请求记录
#[derive(Debug, Clone)]
pub struct SlowRequest {
    pub path: String,
    pub method: String,
    pub status: u16,
    pub duration_ms: u64,
    pub client_ip: String,
    pub timestamp: u64,
}

/// 对请求记录列表执行分析，生成分析报告
///
/// - `top_n`: 慢请求和热点路径各返回前 N 条
/// - `slow_threshold_ms`: 慢请求判定阈值（毫秒）
pub fn analyze(records: &[RequestRecord], top_n: usize, slow_threshold_ms: u64) -> AnalysisReport {
    if records.is_empty() {
        return AnalysisReport {
            total_requests: 0,
            avg_duration_ms: 0.0,
            slow_requests: vec![],
            hot_paths: vec![],
            status_distribution: HashMap::new(),
            total_bytes: 0,
            error_rate_5xx: 0.0,
        };
    }

    let total = records.len();
    let mut total_duration_ms: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut path_counts: HashMap<String, u64> = HashMap::new();
    let mut status_counts: HashMap<u16, u64> = HashMap::new();
    let mut slow_list: Vec<SlowRequest> = Vec::new();
    let mut error_5xx: u64 = 0;

    for rec in records {
        let dur_ms = rec.duration.as_millis() as u64;
        total_duration_ms += dur_ms;
        total_bytes += rec.bytes_sent;

        // 路径计数
        *path_counts.entry(rec.path.clone()).or_insert(0) += 1;
        // 状态码计数
        *status_counts.entry(rec.status).or_insert(0) += 1;

        if rec.status >= 500 {
            error_5xx += 1;
        }

        // 慢请求收集
        if dur_ms >= slow_threshold_ms {
            slow_list.push(SlowRequest {
                path: rec.path.clone(),
                method: rec.method.clone(),
                status: rec.status,
                duration_ms: dur_ms,
                client_ip: rec.client_ip.clone(),
                timestamp: rec.timestamp,
            });
        }
    }

    // 按耗时降序排列慢请求，取 top_n
    slow_list.sort_by(|a, b| b.duration_ms.cmp(&a.duration_ms));
    slow_list.truncate(top_n);

    // 按请求次数降序排列热点路径，取 top_n
    let mut hot_paths: Vec<(String, u64)> = path_counts.into_iter().collect();
    hot_paths.sort_by(|a, b| b.1.cmp(&a.1));
    hot_paths.truncate(top_n);

    let avg_duration_ms = total_duration_ms as f64 / total as f64;
    let error_rate_5xx = error_5xx as f64 / total as f64;

    AnalysisReport {
        total_requests: total,
        avg_duration_ms,
        slow_requests: slow_list,
        hot_paths,
        status_distribution: status_counts,
        total_bytes,
        error_rate_5xx,
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(path: &str, status: u16, dur_ms: u64) -> RequestRecord {
        RequestRecord {
            timestamp: 0,
            site: "demo".into(),
            path: path.to_string(),
            method: "GET".into(),
            status,
            duration: Duration::from_millis(dur_ms),
            bytes_sent: 100,
            client_ip: "127.0.0.1".into(),
        }
    }

    #[test]
    fn test_empty_records() {
        let report = analyze(&[], 5, 1000);
        assert_eq!(report.total_requests, 0);
        assert_eq!(report.avg_duration_ms, 0.0);
    }

    #[test]
    fn test_slow_request_detection() {
        let records = vec![
            rec("/fast", 200, 50),
            rec("/slow", 200, 2000),
            rec("/medium", 200, 500),
        ];
        let report = analyze(&records, 10, 1000);
        assert_eq!(report.slow_requests.len(), 1);
        assert_eq!(report.slow_requests[0].path, "/slow");
    }

    #[test]
    fn test_hot_paths_sorted() {
        let records = vec![
            rec("/api", 200, 10),
            rec("/api", 200, 10),
            rec("/api", 200, 10),
            rec("/home", 200, 10),
            rec("/home", 200, 10),
        ];
        let report = analyze(&records, 5, 9999);
        assert_eq!(report.hot_paths[0].0, "/api");
        assert_eq!(report.hot_paths[0].1, 3);
        assert_eq!(report.hot_paths[1].0, "/home");
        assert_eq!(report.hot_paths[1].1, 2);
    }

    #[test]
    fn test_status_distribution() {
        let records = vec![
            rec("/a", 200, 1),
            rec("/b", 200, 1),
            rec("/c", 404, 1),
            rec("/d", 500, 1),
        ];
        let report = analyze(&records, 5, 9999);
        assert_eq!(*report.status_distribution.get(&200).unwrap(), 2);
        assert_eq!(*report.status_distribution.get(&404).unwrap(), 1);
        assert_eq!(*report.status_distribution.get(&500).unwrap(), 1);
    }

    #[test]
    fn test_error_rate_5xx() {
        let records = vec![
            rec("/a", 200, 1),
            rec("/b", 500, 1),
            rec("/c", 502, 1),
            rec("/d", 200, 1),
        ];
        let report = analyze(&records, 5, 9999);
        assert!((report.error_rate_5xx - 0.5).abs() < 1e-9);
    }

    #[test]
    fn test_top_n_limit() {
        let records: Vec<RequestRecord> = (0..20)
            .map(|i| rec(&format!("/path{}", i), 200, i * 10))
            .collect();
        let report = analyze(&records, 3, 0);
        assert_eq!(report.slow_requests.len(), 3);
        assert_eq!(report.hot_paths.len(), 3);
    }
}
