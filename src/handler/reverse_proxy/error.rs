//! 反向代理结构化错误类型
//!
//! 用 thiserror 定义细分错误，使日志/监控能精确区分：
//!   - 连接拒绝（上游未启动 / 端口错误）
//!   - 连接超时（网络慢 / 防火墙丢包）
//!   - 读取超时（上游处理慢）
//!   - 写入超时（客户端上传慢）
//!   - 连接重置（上游崩溃 / keepalive 过期）
//!   - TLS 握手失败
//!   - 上游返回 5xx（Bad Gateway）
//!   - 协议解析错误

use thiserror::Error;

/// 反向代理请求错误
#[derive(Debug, Error)]
pub enum ProxyError {
    /// 上游拒绝连接（Connection refused）
    /// 通常是上游服务未启动或端口配置错误
    #[error("上游拒绝连接: {addr}")]
    ConnRefused { addr: String },

    /// 连接上游超时
    /// 通常是网络慢、防火墙丢包或上游负载过高
    #[error("连接上游超时 ({timeout_secs}s): {addr}")]
    ConnTimeout { addr: String, timeout_secs: u64 },

    /// 读取上游响应超时
    #[error("读取上游响应超时 ({timeout_secs}s): {addr}")]
    ReadTimeout { addr: String, timeout_secs: u64 },

    /// 写入请求体到上游超时
    #[error("写入请求到上游超时 ({timeout_secs}s): {addr}")]
    WriteTimeout { addr: String, timeout_secs: u64 },

    /// 等待上游首个字节超时（TTFB 超时）
    #[error("等待上游首字节超时: {addr}")]
    TtfbTimeout { addr: String },

    /// 连接被上游重置（RST / EOF before headers）
    /// 通常是 keepalive 连接在服务端被关闭后仍被复用
    #[error("上游重置连接: {addr}")]
    ConnReset { addr: String },

    /// TLS 握手失败
    #[error("TLS 握手失败 ({addr}): {reason}")]
    TlsHandshake { addr: String, reason: String },

    /// 上游返回 5xx 错误（502/503/504 等）
    #[error("上游返回 {status}: {addr}")]
    BadGateway { addr: String, status: u16 },

    /// HTTP 响应行格式错误（非法状态码等）
    #[error("上游响应格式错误 ({addr}): {detail}")]
    InvalidResponse { addr: String, detail: String },

    /// 所有上游节点均不可用（负载均衡无可选节点）
    #[error("所有上游节点不可用，upstream: {upstream}")]
    NoAvailableUpstream { upstream: String },

    /// 其他 IO 错误
    #[error("上游 IO 错误 ({addr}): {source}")]
    Io { addr: String, #[source] source: std::io::Error },
}

impl ProxyError {
    /// 对应的 HTTP 状态码（用于返回给客户端）
    pub fn http_status(&self) -> u16 {
        match self {
            Self::ConnRefused { .. }      => 502, // Bad Gateway
            Self::ConnTimeout { .. }      => 504, // Gateway Timeout
            Self::ReadTimeout { .. }      => 504,
            Self::WriteTimeout { .. }     => 504,
            Self::TtfbTimeout { .. }      => 504,
            Self::ConnReset { .. }        => 502,
            Self::TlsHandshake { .. }     => 502,
            Self::BadGateway { status, .. } => *status,
            Self::InvalidResponse { .. }  => 502,
            Self::NoAvailableUpstream { .. } => 503, // Service Unavailable
            Self::Io { .. }              => 502,
        }
    }

    /// 从 IO 错误推断具体的 ProxyError 类型
    pub fn from_io(addr: &str, e: std::io::Error, context: IoContext) -> Self {
        use std::io::ErrorKind;
        match e.kind() {
            ErrorKind::ConnectionRefused => Self::ConnRefused { addr: addr.to_string() },
            ErrorKind::ConnectionReset | ErrorKind::BrokenPipe | ErrorKind::UnexpectedEof => {
                Self::ConnReset { addr: addr.to_string() }
            }
            _ => match context {
                IoContext::Connect => Self::ConnRefused { addr: addr.to_string() },
                IoContext::Read    => Self::Io { addr: addr.to_string(), source: e },
                IoContext::Write   => Self::Io { addr: addr.to_string(), source: e },
            }
        }
    }
}

/// IO 操作上下文（用于 from_io 推断错误类型）
#[derive(Debug, Clone, Copy)]
pub enum IoContext {
    Connect,
    Read,
    Write,
}
