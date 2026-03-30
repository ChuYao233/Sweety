//! 访问日志中间件
//! 负责：请求完成后记录访问日志，支持 JSON 和 Apache Combined 两种格式
//!
//! # 架构
//! - 请求侧：`logger.send(entry)` — 非阻塞 `try_send` 到 channel，零 spawn，零锁
//! - 写入侧：单一后台 task，用 `BufWriter` 批量刷盘，每 1024 条或 1 秒 flush 一次

use std::path::PathBuf;

use chrono::Local;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

/// 日志格式
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogFormat {
    /// JSON 格式（结构化，适合 ELK/Loki 采集）
    Json,
    /// Apache Combined 格式（兼容传统日志分析工具）
    Combined,
}

/// 单条访问日志记录
#[derive(Debug, Clone)]
pub struct AccessLogEntry {
    /// 客户端 IP
    pub client_ip: String,
    /// 请求方法
    pub method: String,
    /// 请求路径（含查询串）
    pub uri: String,
    /// HTTP 协议版本
    pub http_version: String,
    /// 响应状态码
    pub status: u16,
    /// 响应体字节数
    pub bytes_sent: u64,
    /// Referer 头
    pub referer: String,
    /// User-Agent 头
    pub user_agent: String,
    /// 请求处理耗时（毫秒）
    pub duration_ms: u64,
    /// 站点名称
    pub site: String,
}

/// 访问日志写入器
///
/// 内部持有一个 mpsc Sender；请求侧调用 `send()` 非阻塞投递，
/// 单一后台 task 负责格式化 + BufWriter 批量写文件。
pub struct AccessLogger {
    tx: Option<mpsc::Sender<AccessLogEntry>>,
    format: LogFormat,
}

impl AccessLogger {
    /// 创建写入文件的日志器，同时启动后台写入 task
    pub fn file_sync(path: &PathBuf, format: LogFormat) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let std_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;

        let (tx, rx) = mpsc::channel::<AccessLogEntry>(4096);
        let fmt = format.clone();

        // 单一后台 task：从 channel 接收日志行，BufWriter 批量写文件
        tokio::spawn(async move {
            writer_task(rx, std_file, fmt).await;
        });

        Ok(Self { tx: Some(tx), format })
    }

    /// 投递一条日志（非阻塞，channel 满则丢弃，不影响请求延迟）
    pub fn send(&self, entry: AccessLogEntry) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(entry);
        }
    }
}

/// 后台写入 task：tokio BufWriter 缓冲 + 定时 flush
async fn writer_task(
    mut rx: mpsc::Receiver<AccessLogEntry>,
    file: std::fs::File,
    format: LogFormat,
) {
    use tokio::time::{Duration, interval};
    let tokio_file = tokio::fs::File::from_std(file);
    let mut buf = tokio::io::BufWriter::with_capacity(256 * 1024, tokio_file);
    let mut ticker = interval(Duration::from_secs(1));
    let mut pending = 0usize;

    loop {
        tokio::select! {
            entry = rx.recv() => {
                match entry {
                    None => break, // 所有 sender drop，退出
                    Some(e) => {
                        let line = format_entry(&e, &format);
                        let bytes = format!("{}\n", line);
                        let _ = buf.write_all(bytes.as_bytes()).await;
                        pending += 1;
                        // 每 1024 条强制 flush 一次
                        if pending >= 1024 {
                            let _ = buf.flush().await;
                            pending = 0;
                        }
                    }
                }
            }
            _ = ticker.tick() => {
                // 每秒 flush 一次，低流量时日志也能及时落盘
                if pending > 0 {
                    let _ = buf.flush().await;
                    pending = 0;
                }
            }
        }
    }
    let _ = buf.flush().await;
}

/// 将日志记录格式化为字符串
fn format_entry(e: &AccessLogEntry, format: &LogFormat) -> String {
    match format {
            LogFormat::Json => {
                // JSON 格式
                serde_json::json!({
                    "time":       Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%z").to_string(),
                    "site":       e.site,
                    "client":     e.client_ip,
                    "method":     e.method,
                    "uri":        e.uri,
                    "proto":      e.http_version,
                    "status":     e.status,
                    "bytes":      e.bytes_sent,
                    "referer":    e.referer,
                    "ua":         e.user_agent,
                    "duration_ms": e.duration_ms,
                })
                .to_string()
            }
            LogFormat::Combined => {
                // Apache Combined 格式：
                // client - - [time] "METHOD uri PROTO" status bytes "referer" "ua" duration_ms
                format!(
                    r#"{} - - [{}] "{} {} {}" {} {} "{}" "{}" {}ms"#,
                    e.client_ip,
                    Local::now().format("%d/%b/%Y:%H:%M:%S %z"),
                    e.method,
                    e.uri,
                    e.http_version,
                    e.status,
                    e.bytes_sent,
                    e.referer,
                    e.user_agent,
                    e.duration_ms,
                )
            }
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> AccessLogEntry {
        AccessLogEntry {
            client_ip:    "127.0.0.1".into(),
            method:       "GET".into(),
            uri:          "/index.html".into(),
            http_version: "HTTP/1.1".into(),
            status:       200,
            bytes_sent:   1024,
            referer:      "-".into(),
            user_agent:   "Mozilla/5.0".into(),
            duration_ms:  12,
            site:         "demo".into(),
        }
    }

    #[test]
    fn test_combined_format_contains_status() {
        let entry = sample_entry();
        let line = format_entry(&entry, &LogFormat::Combined);
        assert!(line.contains("200"));
        assert!(line.contains("GET"));
        assert!(line.contains("/index.html"));
    }

    #[test]
    fn test_json_format_is_valid() {
        let entry = sample_entry();
        let line = format_entry(&entry, &LogFormat::Json);
        let v: serde_json::Value = serde_json::from_str(&line).expect("JSON 格式无效");
        assert_eq!(v["status"], 200);
        assert_eq!(v["method"], "GET");
    }

    #[tokio::test]
    async fn test_file_logger_writes() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("access.log");
        let logger = AccessLogger::file_sync(&log_path, LogFormat::Combined).unwrap();
        logger.send(sample_entry());
        // 等后台 task flush（最多 1.5 秒）
        tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;
        let content = tokio::fs::read_to_string(&log_path).await.unwrap();
        assert!(!content.is_empty());
        assert!(content.contains("200"));
    }
}
