//! auth_request 子请求鉴权模块
//! 等价 Nginx `auth_request /auth;`
//!
//! 工作原理：
//! 1. 每个请求到达时，先向 `auth_request` URL 发一个 GET 子请求（携带原始请求头）
//! 2. 鉴权服务返回 2xx → 继续处理原始请求，并将鉴权响应头注入原始请求
//! 3. 非 2xx → 直接返回 `auth_failure_status`（默认 401）

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing::debug;

use crate::config::model::HeaderOverride;

/// 子请求鉴权结果
pub enum AuthResult {
    /// 鉴权通过，携带鉴权响应头（可注入到原始请求）
    Allow(Vec<(String, String)>),
    /// 鉴权失败，应返回此状态码
    Deny(u16),
}

/// 执行 auth_request 子请求
///
/// - `auth_url`：完整鉴权 URL，如 `http://127.0.0.1:8080/auth` 或相对路径 `/auth`
/// - `original_headers`：原始请求的头部（透传 Cookie、Authorization 等给鉴权服务）
/// - `client_ip`：客户端 IP（注入 X-Real-IP）
/// - `extra_headers`：`auth_request_headers` 列表（额外注入子请求的头）
/// - `failure_status`：鉴权失败时返回的 HTTP 状态码
pub async fn check(
    auth_url: &str,
    original_headers: &[(String, String)],
    client_ip: &str,
    extra_headers: &[HeaderOverride],
    failure_status: u16,
) -> AuthResult {
    match do_auth_request(auth_url, original_headers, client_ip, extra_headers).await {
        Ok((status, resp_headers)) if (200..300).contains(&(status as u32)) => {
            debug!("auth_request 通过: {} → {}", auth_url, status);
            AuthResult::Allow(resp_headers)
        }
        Ok((status, _)) => {
            debug!("auth_request 拒绝: {} → {}", auth_url, status);
            AuthResult::Deny(failure_status)
        }
        Err(e) => {
            tracing::warn!("auth_request 子请求失败 {}: {}", auth_url, e);
            // 鉴权服务不可达时，保守拒绝（安全第一）
            AuthResult::Deny(failure_status)
        }
    }
}

/// 内部：发送 auth 子请求，返回 (状态码, 响应头列表)
async fn do_auth_request(
    auth_url: &str,
    original_headers: &[(String, String)],
    client_ip: &str,
    extra_headers: &[HeaderOverride],
) -> anyhow::Result<(u16, Vec<(String, String)>)> {
    // 解析 auth_url：支持完整 URL 和相对路径
    let (host, port, path, use_tls) = parse_auth_url(auth_url)?;

    let addr = format!("{}:{}", host, port);
    let tcp = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        TcpStream::connect(&addr),
    ).await
    .map_err(|_| anyhow::anyhow!("auth_request 连接超时: {}", addr))??;

    // 构造 GET 子请求
    let mut req = format!("GET {} HTTP/1.1\r\nHost: {}\r\n", path, host);

    // 透传安全相关头（Cookie、Authorization、X-Forwarded-For 等）
    for (k, v) in original_headers {
        let kl = k.to_lowercase();
        // 只透传认证相关头，不透传 body 相关头
        if matches!(kl.as_str(), "cookie" | "authorization" | "x-forwarded-for"
            | "x-real-ip" | "x-forwarded-proto" | "accept" | "accept-language") {
            req.push_str(&format!("{}: {}\r\n", k, v));
        }
    }

    // 注入 extra_headers（auth_request_headers 配置）
    for h in extra_headers {
        let val = h.value.replace("$remote_addr", client_ip);
        req.push_str(&format!("{}: {}\r\n", h.name, val));
    }

    req.push_str(&format!("X-Real-IP: {}\r\n", client_ip));
    req.push_str("X-Auth-Request: 1\r\n");
    req.push_str("Connection: close\r\n");
    req.push_str("Content-Length: 0\r\n\r\n");

    if use_tls {
        // TLS 鉴权端点
        use crate::handler::reverse_proxy::tls_client::tls_connect;
        let tls = tls_connect(tcp, &host, false).await?;
        let (r, mut w) = tokio::io::split(tls);
        w.write_all(req.as_bytes()).await?;
        w.flush().await?;
        let mut buf = BufReader::new(r);
        read_auth_response(&mut buf).await
    } else {
        let (r, mut w) = tokio::io::split(tcp);
        w.write_all(req.as_bytes()).await?;
        w.flush().await?;
        let mut buf = BufReader::new(r);
        read_auth_response(&mut buf).await
    }
}

/// 读取 auth 响应（只需要状态码 + 响应头，不读 body）
async fn read_auth_response<R>(
    buf: &mut BufReader<R>,
) -> anyhow::Result<(u16, Vec<(String, String)>)>
where R: tokio::io::AsyncRead + Unpin {
    let mut status_line = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        buf.read_line(&mut status_line),
    ).await
    .map_err(|_| anyhow::anyhow!("auth_request 响应超时"))??;

    let status = parse_status_u16(&status_line);

    // 读取响应头
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        match buf.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() { break; }
        if let Some(colon) = trimmed.find(':') {
            let k = trimmed[..colon].trim().to_string();
            let v = trimmed[colon + 1..].trim().to_string();
            headers.push((k, v));
        }
    }

    Ok((status, headers))
}

/// 解析 auth_url，返回 (host, port, path, use_tls)
/// 支持：
/// - `http://127.0.0.1:8080/auth`
/// - `https://auth.internal/check`
/// - `/auth`（本地回环，localhost:80）
fn parse_auth_url(url: &str) -> anyhow::Result<(String, u16, String, bool)> {
    if url.starts_with("http://") || url.starts_with("https://") {
        let use_tls = url.starts_with("https://");
        let without_scheme = if use_tls { &url[8..] } else { &url[7..] };
        let (authority, path) = match without_scheme.find('/') {
            Some(idx) => (&without_scheme[..idx], &without_scheme[idx..]),
            None => (without_scheme, "/"),
        };
        let (host, port) = if let Some(colon) = authority.rfind(':') {
            let port: u16 = authority[colon + 1..].parse()
                .map_err(|_| anyhow::anyhow!("auth_url 端口解析失败: {}", url))?;
            (authority[..colon].to_string(), port)
        } else {
            (authority.to_string(), if use_tls { 443 } else { 80 })
        };
        Ok((host, port, path.to_string(), use_tls))
    } else if url.starts_with('/') {
        // 相对路径：向本地 127.0.0.1:80 发请求
        Ok(("127.0.0.1".to_string(), 80, url.to_string(), false))
    } else {
        Err(anyhow::anyhow!("无法解析 auth_request URL: {}", url))
    }
}

/// 从 HTTP 状态行提取状态码（"HTTP/1.1 200 OK" → 200）
fn parse_status_u16(line: &str) -> u16 {
    line.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(500)
}
