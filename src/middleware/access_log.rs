//! 访问日志中间件
//! 负责：请求完成后记录访问日志，支持 JSON 和 Apache Combined 两种格式

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Local;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::error;

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

/// 访问日志写入器（异步，带内部缓冲）
pub struct AccessLogger {
    /// 日志文件写入器（None 表示仅输出到 tracing）
    writer: Option<Arc<Mutex<tokio::fs::File>>>,
    /// 日志格式
    format: LogFormat,
}

impl AccessLogger {
    /// 创建仅输出到 tracing 的日志器
    pub fn stdout(format: LogFormat) -> Self {
        Self { writer: None, format }
    }

    /// 创建写入文件的日志器
    pub async fn file(path: &PathBuf, format: LogFormat) -> anyhow::Result<Self> {
        // 创建目录（如果不存在）
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        Ok(Self {
            writer: Some(Arc::new(Mutex::new(file))),
            format,
        })
    }

    /// 写入一条访问日志
    pub async fn write(&self, entry: &AccessLogEntry) {
        let line = self.format_entry(entry);

        // 写入文件
        if let Some(writer) = &self.writer {
            let mut file = writer.lock().await;
            let line_with_newline = format!("{}\n", line);
            if let Err(e) = file.write_all(line_with_newline.as_bytes()).await {
                error!("访问日志写入失败: {}", e);
            }
        }

        // 同时通过 tracing 输出（便于开发调试）
        tracing::info!(target: "access_log", "{}", line);
    }

    /// 将日志记录格式化为字符串
    fn format_entry(&self, e: &AccessLogEntry) -> String {
        match self.format {
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
        let logger = AccessLogger::stdout(LogFormat::Combined);
        let line = logger.format_entry(&sample_entry());
        assert!(line.contains("200"));
        assert!(line.contains("GET"));
        assert!(line.contains("/index.html"));
    }

    #[test]
    fn test_json_format_is_valid() {
        let logger = AccessLogger::stdout(LogFormat::Json);
        let line = logger.format_entry(&sample_entry());
        let v: serde_json::Value = serde_json::from_str(&line).expect("JSON 格式无效");
        assert_eq!(v["status"], 200);
        assert_eq!(v["method"], "GET");
    }

    #[tokio::test]
    async fn test_file_logger_writes() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("access.log");
        let logger = AccessLogger::file(&log_path, LogFormat::Combined).await.unwrap();
        logger.write(&sample_entry()).await;
        // 确认文件非空
        let content = tokio::fs::read_to_string(&log_path).await.unwrap();
        assert!(!content.is_empty());
        assert!(content.contains("200"));
    }
}
