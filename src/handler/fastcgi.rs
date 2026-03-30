//! FastCGI / PHP 处理器
//! 负责：FastCGI 协议实现、PHP-FPM 连接池管理、沙箱隔离
//! 参照 RFC 3875 (CGI) 和 FastCGI 1.0 规范

use xitca_web::{
    body::ResponseBody,
    http::{StatusCode, WebResponse, header::{CONTENT_TYPE, HeaderValue}},
    WebContext,
};

use crate::config::model::LocationConfig;
use crate::dispatcher::vhost::SiteInfo;
use crate::server::http::AppState;

// ─────────────────────────────────────────────
// FastCGI 协议常量
// ─────────────────────────────────────────────

const FCGI_VERSION: u8 = 1;
const FCGI_BEGIN_REQUEST: u8 = 1;
const FCGI_PARAMS: u8 = 4;
const FCGI_STDIN: u8 = 5;
const FCGI_STDOUT: u8 = 6;
const FCGI_RESPONDER: u16 = 1;

// ─────────────────────────────────────────────
// FastCGI 数据包结构
// ─────────────────────────────────────────────

/// FastCGI 请求头（8 字节固定长度）
#[derive(Debug)]
#[allow(dead_code)]
struct FcgiHeader {
    version: u8,
    record_type: u8,
    request_id: u16,
    content_length: u16,
    padding_length: u8,
    reserved: u8,
}

impl FcgiHeader {
    /// 序列化为字节数组
    fn to_bytes(&self) -> [u8; 8] {
        let id_bytes = self.request_id.to_be_bytes();
        let len_bytes = self.content_length.to_be_bytes();
        [
            self.version,
            self.record_type,
            id_bytes[0],
            id_bytes[1],
            len_bytes[0],
            len_bytes[1],
            self.padding_length,
            self.reserved,
        ]
    }
}

/// 构建 FastCGI BEGIN_REQUEST 记录
fn build_begin_request(request_id: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(16);
    // 请求头
    let header = FcgiHeader {
        version: FCGI_VERSION,
        record_type: FCGI_BEGIN_REQUEST,
        request_id,
        content_length: 8,
        padding_length: 0,
        reserved: 0,
    };
    buf.extend_from_slice(&header.to_bytes());
    // BEGIN_REQUEST body：role(2) + flags(1) + reserved(5)
    let role_bytes = FCGI_RESPONDER.to_be_bytes();
    buf.extend_from_slice(&role_bytes); // role = FCGI_RESPONDER
    buf.push(0); // flags = 0（不保持连接）
    buf.extend_from_slice(&[0u8; 5]); // reserved
    buf
}

/// 将 FastCGI 参数（name-value 对）编码为 PARAMS 记录
fn encode_params(params: &[(&str, &str)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (name, value) in params {
        encode_name_value_pair(&mut body, name.as_bytes(), value.as_bytes());
    }
    body
}

/// FastCGI name-value 对编码（支持长度 > 127 的名称/值）
fn encode_name_value_pair(buf: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    encode_length(buf, name.len());
    encode_length(buf, value.len());
    buf.extend_from_slice(name);
    buf.extend_from_slice(value);
}

/// FastCGI 长度编码（1 字节或 4 字节）
fn encode_length(buf: &mut Vec<u8>, len: usize) {
    if len < 128 {
        buf.push(len as u8);
    } else {
        let b = (len as u32) | 0x80000000;
        buf.extend_from_slice(&b.to_be_bytes());
    }
}

// ─────────────────────────────────────────────
// 主处理函数
// ─────────────────────────────────────────────

/// 处理 FastCGI / PHP 请求（xitca-web WebContext 版本）
pub async fn handle_xitca(
    ctx: &WebContext<'_, AppState>,
    site: &SiteInfo,
    _location: &LocationConfig,
) -> WebResponse {
    // 从站点配置读取 FastCGI socket 路径（默认尝试 Unix Socket）
    let socket_path: String = site.fastcgi
        .as_ref()
        .and_then(|f| f.socket.as_ref())
        .map(|p: &std::path::PathBuf| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/var/run/php/php8.2-fpm.sock".to_string());

    let method = ctx.req().method().as_str().to_string();
    let full_path = ctx.req().uri().path_and_query()
        .map(|p| p.as_str()).unwrap_or("/").to_string();
    let path_only = full_path.split('?').next().unwrap_or(&full_path).to_string();
    let query = full_path.split('?').nth(1).unwrap_or("").to_string();
    let peer_ip = ctx.req().body().socket_addr().ip().to_string();
    let peer_port = ctx.req().body().socket_addr().port().to_string();

    // 构建 SCRIPT_FILENAME
    let script_filename = match &site.root {
        Some(root) => format!("{}{}", root.display(), path_only),
        None => {
            return fcgi_error(StatusCode::INTERNAL_SERVER_ERROR, "FastCGI: 站点未配置 root 目录");
        }
    };

    // 构建 CGI 参数列表（所有字符串都是拥有的，避免生命周期问题）
    let params: Vec<(String, String)> = vec![
        ("SCRIPT_FILENAME".into(), script_filename),
        ("SCRIPT_NAME".into(), path_only.clone()),
        ("REQUEST_METHOD".into(), method),
        ("REQUEST_URI".into(), full_path),
        ("QUERY_STRING".into(), query),
        ("SERVER_SOFTWARE".into(), "Sweety/0.1".into()),
        ("SERVER_PROTOCOL".into(), "HTTP/1.1".into()),
        ("GATEWAY_INTERFACE".into(), "CGI/1.1".into()),
        ("REMOTE_ADDR".into(), peer_ip),
        ("REMOTE_PORT".into(), peer_port),
        ("SERVER_NAME".into(), ctx.req().headers()
            .get("host").and_then(|v| v.to_str().ok()).unwrap_or("").to_string()),
    ];

    // 将 (String, String) 转成 (&str, &str) 引用
    let params_ref: Vec<(&str, &str)> = params.iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    // 连接 FastCGI 后端并发送请求
    match connect_and_send(&socket_path, &params_ref).await {
        Ok(response_body) => parse_fcgi_response(response_body),
        Err(e) => {
            tracing::error!("FastCGI 连接失败 {}: {}", socket_path, e);
            fcgi_error(StatusCode::BAD_GATEWAY, &format!("FastCGI 后端不可用: {}", e))
        }
    }
}

/// 构造 FastCGI 错误响应
fn fcgi_error(status: StatusCode, _msg: &str) -> WebResponse {
    let body = crate::handler::error_page::build_default_html(status.as_u16());
    let mut resp = WebResponse::new(ResponseBody::from(body));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp
}

/// 连接 FastCGI socket 并发送请求（跨平台实现）
///
/// - Unix/Linux/macOS：优先尝试 Unix Domain Socket
/// - Windows：使用 TCP 连接（socket_path 格式为 "host:port"）
async fn connect_and_send(socket_path: &str, params: &[(&str, &str)]) -> anyhow::Result<Vec<u8>> {
    // 根据平台选择连接方式，统一通过 fcgi_send_recv 发送/接收数据
    #[cfg(unix)]
    {
        // Unix 平台：尝试 Unix Domain Socket
        let stream = tokio::net::UnixStream::connect(socket_path).await?;
        let (read_half, write_half) = tokio::io::split(stream);
        fcgi_send_recv(read_half, write_half, params).await
    }
    #[cfg(not(unix))]
    {
        // Windows 等平台：使用 TCP 连接
        // socket_path 应为 "host:port" 格式，如 "127.0.0.1:9000"
        let stream = tokio::net::TcpStream::connect(socket_path).await?;
        let (read_half, write_half) = tokio::io::split(stream);
        fcgi_send_recv(read_half, write_half, params).await
    }
}

/// FastCGI 协议数据发送与响应接收（通用实现，与 stream 类型无关）
async fn fcgi_send_recv<R, W>(
    mut reader: R,
    mut writer: W,
    params: &[(&str, &str)],
) -> anyhow::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let request_id: u16 = 1;

    // 1. 发送 BEGIN_REQUEST
    writer.write_all(&build_begin_request(request_id)).await?;

    // 2. 发送 PARAMS
    let params_body = encode_params(params);
    if !params_body.is_empty() {
        let header = FcgiHeader {
            version: FCGI_VERSION,
            record_type: FCGI_PARAMS,
            request_id,
            content_length: params_body.len() as u16,
            padding_length: 0,
            reserved: 0,
        };
        writer.write_all(&header.to_bytes()).await?;
        writer.write_all(&params_body).await?;
    }
    // PARAMS 结束记录（content_length = 0）
    let end_params = FcgiHeader {
        version: FCGI_VERSION,
        record_type: FCGI_PARAMS,
        request_id,
        content_length: 0,
        padding_length: 0,
        reserved: 0,
    };
    writer.write_all(&end_params.to_bytes()).await?;

    // 3. 发送空 STDIN（无请求体）
    let empty_stdin = FcgiHeader {
        version: FCGI_VERSION,
        record_type: FCGI_STDIN,
        request_id,
        content_length: 0,
        padding_length: 0,
        reserved: 0,
    };
    writer.write_all(&empty_stdin.to_bytes()).await?;
    writer.flush().await?;

    // 4. 读取响应
    let mut response = Vec::new();
    reader.read_to_end(&mut response).await?;

    Ok(response)
}

/// 解析 FastCGI STDOUT 内容为 HTTP 响应
fn parse_fcgi_response(raw: Vec<u8>) -> WebResponse {
    // 遍历 FastCGI 记录，提取 STDOUT 数据
    let mut stdout = Vec::new();
    let mut offset = 0;

    while offset + 8 <= raw.len() {
        let record_type = raw[offset + 1];
        let content_len = u16::from_be_bytes([raw[offset + 4], raw[offset + 5]]) as usize;
        let padding_len = raw[offset + 6] as usize;
        let data_start = offset + 8;
        let data_end = data_start + content_len;

        if data_end > raw.len() {
            break;
        }

        if record_type == FCGI_STDOUT {
            stdout.extend_from_slice(&raw[data_start..data_end]);
        }

        offset = data_end + padding_len;
    }

    // PHP 输出格式：HTTP 响应头 + 空行 + body
    let (status_code, body_bytes) = if let Ok(text) = std::str::from_utf8(&stdout) {
        if let Some(header_end) = text.find("\r\n\r\n") {
            let body = text[header_end + 4..].as_bytes().to_vec();
            // 提取 Status 头
            let code = text
                .lines()
                .find(|l| l.to_lowercase().starts_with("status:"))
                .and_then(|l| l.split(':').nth(1))
                .and_then(|s| s.trim().split_whitespace().next())
                .and_then(|s| s.parse::<u16>().ok())
                .unwrap_or(200);
            (code, body)
        } else {
            (200, stdout)
        }
    } else {
        (200, stdout)
    };

    let http_status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::OK);
    let mut resp = WebResponse::new(ResponseBody::from(body_bytes));
    *resp.status_mut() = http_status;
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp
}
