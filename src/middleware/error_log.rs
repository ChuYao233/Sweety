//! 错误日志中间件
//! 负责：捕获 Handler 处理过程中的错误，写入错误日志文件

use std::path::PathBuf;
use std::sync::Arc;

use chrono::Local;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::error;

/// 错误日志级别
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorLevel {
    Error,
    Warn,
    Notice,
}

impl std::fmt::Display for ErrorLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErrorLevel::Error  => write!(f, "error"),
            ErrorLevel::Warn   => write!(f, "warn"),
            ErrorLevel::Notice => write!(f, "notice"),
        }
    }
}

/// 错误日志写入器
pub struct ErrorLogger {
    /// 日志文件写入器（None = 仅通过 tracing 输出）
    writer: Option<Arc<Mutex<tokio::fs::File>>>,
}

impl ErrorLogger {
    /// 创建仅通过 tracing 输出的日志器（开发调试用）
    pub fn tracing_only() -> Self {
        Self { writer: None }
    }

    /// 创建写入文件的日志器
    pub async fn file(path: &PathBuf) -> anyhow::Result<Self> {
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
        })
    }

    /// 记录一条错误日志
    pub async fn log(&self, level: ErrorLevel, site: &str, message: &str) {
        let line = format!(
            "{} [{}] [{}] {}",
            Local::now().format("%Y/%m/%d %H:%M:%S"),
            level,
            site,
            message,
        );

        if let Some(writer) = &self.writer {
            let mut file = writer.lock().await;
            if let Err(e) = file.write_all(format!("{}\n", line).as_bytes()).await {
                error!("错误日志写入失败: {}", e);
            }
        }

        match level {
            ErrorLevel::Error  => error!(target: "error_log", "{}", line),
            ErrorLevel::Warn   => tracing::warn!(target: "error_log", "{}", line),
            ErrorLevel::Notice => tracing::info!(target: "error_log", "{}", line),
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
    async fn test_tracing_only_logger_no_panic() {
        let logger = ErrorLogger::tracing_only();
        logger.log(ErrorLevel::Error, "demo", "测试错误消息").await;
    }

    #[tokio::test]
    async fn test_file_logger_writes() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("error.log");
        let logger = ErrorLogger::file(&log_path).await.unwrap();
        logger.log(ErrorLevel::Error, "demo", "文件写入测试").await;
        let content = tokio::fs::read_to_string(&log_path).await.unwrap();
        assert!(content.contains("文件写入测试"));
        assert!(content.contains("[error]"));
        assert!(content.contains("[demo]"));
    }
}
