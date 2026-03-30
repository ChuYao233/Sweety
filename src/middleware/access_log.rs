//! 访问日志中间件
//! 负责：请求完成后记录访问日志，支持 JSON 和 Apache Combined 两种格式
//!
//! # 架构
//! - 请求侧：`logger.send(entry)` — 非阻塞 `try_send`，零锁，零 spawn
//! - 写入侧：独立系统线程（不占 tokio worker），std::io::BufWriter 批量刷盘
//! - 可在 tokio runtime 启动前安全创建

use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use chrono::Local;

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
/// 请求侧 `try_send` 非阻塞投递；独立系统线程负责写文件，不占 tokio worker。
/// 可在 tokio runtime 启动前安全调用 `file_sync`。
pub struct AccessLogger {
    tx: Option<std_mpsc::SyncSender<AccessLogEntry>>,
}

impl AccessLogger {
    /// 创建写文件日志器，同时启动独立写入线程
    pub fn file_sync(path: &PathBuf, format: LogFormat) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;

        // 容量 4096：满时 try_send 直接丢弃，不阻塞请求
        let (tx, rx) = std_mpsc::sync_channel::<AccessLogEntry>(4096);

        // 独立系统线程：完全不依赖 tokio runtime
        std::thread::Builder::new()
            .name("access-log-writer".into())
            .spawn(move || writer_thread(rx, file, format))?;

        Ok(Self { tx: Some(tx) })
    }

    /// 投递一条日志（非阻塞，channel 满则丢弃，不影响请求延迟）
    pub fn send(&self, entry: AccessLogEntry) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(entry);
        }
    }
}

/// 后台写入线程：BufWriter 256 KiB 缓冲 + 每 1024 条或每秒 flush 一次
fn writer_thread(
    rx: std_mpsc::Receiver<AccessLogEntry>,
    file: std::fs::File,
    format: LogFormat,
) {
    let mut buf = BufWriter::with_capacity(256 * 1024, file);
    let mut pending = 0usize;
    let flush_interval = Duration::from_secs(1);

    loop {
        // 带超时的 recv：超时后触发定时 flush
        match rx.recv_timeout(flush_interval) {
            Ok(e) => {
                let line = format_entry(&e, &format);
                let _ = writeln!(buf, "{}", line);
                pending += 1;
                // 每 1024 条强制 flush 一次
                if pending >= 1024 {
                    let _ = buf.flush();
                    pending = 0;
                }
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                // 每秒 flush 一次，低流量时日志也能及时落盘
                if pending > 0 {
                    let _ = buf.flush();
                    pending = 0;
                }
            }
            Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                // 所有 sender drop（进程退出），flush 后退出
                break;
            }
        }
    }
    let _ = buf.flush();
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

    #[test]
    fn test_file_logger_writes() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("access.log");
        let logger = AccessLogger::file_sync(&log_path, LogFormat::Combined).unwrap();
        logger.send(sample_entry());
        // 等写入线程 flush（最多 1.5 秒，超过定时 flush 间隔）
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let content = std::fs::read_to_string(&log_path).unwrap();
        assert!(!content.is_empty());
        assert!(content.contains("200"));
    }
}
