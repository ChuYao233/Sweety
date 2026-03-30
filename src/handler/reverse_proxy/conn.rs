//! HTTP 连接层
//! 负责：与上游建立连接、发送请求、读取响应（支持 chunked/gzip）、健康检查探活

use std::time::Duration;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing::debug;
use xitca_web::{
    body::ResponseBody,
    http::{StatusCode, WebResponse, header::{CONTENT_LENGTH, HeaderValue}},
};

use super::response::{apply_response_headers, parse_status_code};
use super::tls_client::tls_connect;

/// 向上游转发请求，自动选择 HTTP/HTTPS（gzip/chunked 全支持）
#[allow(clippy::too_many_arguments)]
pub async fn forward_request(
    upstream_addr: &str,
    method: &str,
    path: &str,
    host: &str,
    use_tls: bool,
    tls_sni: &str,
    tls_insecure: bool,
    extra_headers: &[(String, String)],
    client_ip: &str,
    body: &[u8],
    is_ws: bool,
    strip_cookie_secure: bool,
    proxy_cookie_domain: Option<&str>,
    proxy_redirect_from: Option<&str>,
    proxy_redirect_to: Option<&str>,
) -> Result<WebResponse> {
    debug!("转发 {} {} → {} (tls={}, body={}B, ws={})",
        method, path, upstream_addr, use_tls, body.len(), is_ws);

    let tcp = tokio::time::timeout(
        Duration::from_secs(10),
        TcpStream::connect(upstream_addr),
    ).await
    .map_err(|_| anyhow::anyhow!("连接上游超时: {}", upstream_addr))??;

    if use_tls {
        let tls = tls_connect(tcp, tls_sni, tls_insecure).await?;
        let (r, w) = tokio::io::split(tls);
        send_recv(r, w, method, path, host, extra_headers, client_ip, body, is_ws,
            strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to).await
    } else {
        let (r, w) = tokio::io::split(tcp);
        send_recv(r, w, method, path, host, extra_headers, client_ip, body, is_ws,
            strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to).await
    }
}

/// HTTP/1.1 请求发送 + 响应读取（支持 chunked、Content-Length、gzip 透传）
#[allow(clippy::too_many_arguments)]
async fn send_recv<R, W>(
    reader: R,
    mut writer: W,
    method: &str,
    path: &str,
    host: &str,
    extra_headers: &[(String, String)],
    client_ip: &str,
    req_body: &[u8],
    is_ws: bool,
    strip_cookie_secure: bool,
    proxy_cookie_domain: Option<&str>,
    proxy_redirect_from: Option<&str>,
    proxy_redirect_to: Option<&str>,
) -> Result<WebResponse>
where
    R: AsyncRead + Unpin,
    W: AsyncWriteExt + Unpin,
{
    // ── 构造请求头 ──────────────────────────────────────────────────────────
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\n");
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("X-Real-IP: {client_ip}\r\n"));
    req.push_str(&format!("X-Forwarded-For: {client_ip}\r\n"));
    req.push_str("X-Forwarded-Proto: https\r\n");
    req.push_str(if is_ws { "Connection: upgrade\r\n" } else { "Connection: close\r\n" });
    req.push_str("\r\n");

    debug!("→ {} {} Host:{} body={}B", method, path, host, req_body.len());

    writer.write_all(req.as_bytes()).await?;
    if !req_body.is_empty() {
        writer.write_all(req_body).await?;
    }
    writer.flush().await?;

    // ── 读取响应头 ──────────────────────────────────────────────────────────
    let mut buf = BufReader::new(reader);

    let mut status_line = String::new();
    match tokio::time::timeout(Duration::from_secs(60), buf.read_line(&mut status_line)).await {
        Err(_) => return Err(anyhow::anyhow!("等待上游响应超时")),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof && !status_line.is_empty() => {}
        Ok(Err(e)) => return Err(e.into()),
        Ok(Ok(_)) => {}
    }

    let status_code = parse_status_code(&status_line);
    tracing::debug!("上游 {} ← {} {}", status_code, method, path);

    let mut resp_content_length: Option<usize> = None;
    let mut resp_is_chunked = false;
    let mut response_headers: Vec<(String, String)> = Vec::new();

    loop {
        let mut line = String::new();
        match buf.read_line(&mut line).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        let trimmed = line.trim();
        if trimmed.is_empty() { break; }
        if let Some(colon) = trimmed.find(':') {
            let k = trimmed[..colon].trim().to_string();
            let v = trimmed[colon + 1..].trim().to_string();
            let kl = k.to_lowercase();
            if kl == "content-length"   { resp_content_length = v.parse().ok(); }
            if kl == "transfer-encoding" && v.to_lowercase().contains("chunked") {
                resp_is_chunked = true;
            }
            response_headers.push((k, v));
        }
    }

    // 101 WebSocket 升级
    if status_code == 101 {
        let mut resp = WebResponse::new(ResponseBody::empty());
        *resp.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
        apply_response_headers(&mut resp, &response_headers,
            strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to);
        return Ok(resp);
    }

    // ── 读取响应体（支持 chunked / Content-Length / EOF）───────────────────
    let body_bytes = if resp_is_chunked {
        read_chunked_body(&mut buf).await?
    } else if let Some(len) = resp_content_length {
        read_exact_body(&mut buf, len).await?
    } else {
        let mut b = Vec::new();
        let _ = buf.read_to_end(&mut b).await; // UnexpectedEof = 正常结束
        b
    };

    // ── URL 替换（仅文本类型，处理上游硬编码 URL 的情况）────────────────────
    let final_body = rewrite_body_urls(body_bytes, &response_headers, proxy_redirect_from, proxy_redirect_to);

    let body_len = final_body.len();
    let http_status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::OK);
    let mut resp = WebResponse::new(ResponseBody::from(final_body));
    *resp.status_mut() = http_status;
    apply_response_headers(&mut resp, &response_headers,
        strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to);

    // 根据实际 body 长度重新设置 Content-Length
    if let Ok(v) = HeaderValue::from_str(&body_len.to_string()) {
        resp.headers_mut().insert(CONTENT_LENGTH, v);
    }

    Ok(resp)
}

// ─────────────────────────────────────────────
// 响应体读取辅助函数
// ─────────────────────────────────────────────

/// 读取固定长度响应体（循环读取，正确处理 TLS UnexpectedEof）
async fn read_exact_body<R>(buf: &mut BufReader<R>, len: usize) -> Result<Vec<u8>>
where R: AsyncRead + Unpin {
    let mut b = Vec::with_capacity(len);
    let mut remaining = len;
    let mut tmp = vec![0u8; 8192];
    let tmp_cap = tmp.len();
    loop {
        if remaining == 0 { break; }
        match buf.read(&mut tmp[..remaining.min(tmp_cap)]).await {
            Ok(0) => break,
            Ok(n) => { b.extend_from_slice(&tmp[..n]); remaining -= n; }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(b)
}

/// 解码 Transfer-Encoding: chunked 响应体
///
/// 格式：`<hex_size>\r\n<data>\r\n` ... `0\r\n\r\n`
pub async fn read_chunked_body<R>(buf: &mut BufReader<R>) -> Result<Vec<u8>>
where R: AsyncRead + Unpin {
    let mut body = Vec::new();
    loop {
        // 读取 chunk size 行（16进制）
        let mut size_line = String::new();
        match buf.read_line(&mut size_line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // 去掉可能的 chunk extension（分号后面的部分）
        let size_str = size_line.trim().split(';').next().unwrap_or("0");
        let chunk_size = usize::from_str_radix(size_str, 16).unwrap_or(0);

        if chunk_size == 0 {
            // 最后一个 chunk，读取并丢弃 trailing headers
            let mut trailer = String::new();
            let _ = buf.read_line(&mut trailer).await;
            break;
        }

        // 读取 chunk 数据
        let mut chunk = vec![0u8; chunk_size];
        let mut offset = 0;
        while offset < chunk_size {
            match buf.read(&mut chunk[offset..]).await {
                Ok(0) => break,
                Ok(n) => offset += n,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
        }
        body.extend_from_slice(&chunk[..offset]);

        // 读取并丢弃 chunk 后面的 \r\n
        let mut crlf = String::new();
        let _ = buf.read_line(&mut crlf).await;
    }
    Ok(body)
}

/// 对文本类型响应体做 URL 替换（处理上游硬编码 URL 的情况）
fn rewrite_body_urls(
    bytes: Vec<u8>,
    headers: &[(String, String)],
    from: Option<&str>,
    to: Option<&str>,
) -> Vec<u8> {
    let (Some(from), Some(to)) = (from, to) else { return bytes; };

    let is_text = headers.iter().any(|(k, v)| {
        k.to_lowercase() == "content-type" && (
            v.contains("json") || v.contains("html") ||
            v.contains("javascript") || v.contains("text")
        )
    });

    if !is_text { return bytes; }

    if let Ok(text) = std::str::from_utf8(&bytes) {
        if text.contains(from) {
            debug!("响应体 URL 替换: {} → {}", from, to);
            return text.replace(from, to).into_bytes();
        }
    }
    bytes
}

/// 健康检查探活（HEAD 请求，支持 HTTP/HTTPS）
pub async fn probe_health(addr: &str, path: &str, use_tls: bool, sni: &str, insecure: bool) -> Result<u16> {
    let tcp = TcpStream::connect(addr).await?;
    let req = format!("HEAD {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    if use_tls {
        let tls = tls_connect(tcp, sni, insecure).await?;
        let (r, mut w) = tokio::io::split(tls);
        w.write_all(req.as_bytes()).await?;
        w.flush().await?;
        let mut buf: BufReader<_> = BufReader::new(r);
        let mut line = String::new();
        buf.read_line(&mut line).await?;
        Ok(parse_status_code(&line))
    } else {
        let (r, mut w) = tokio::io::split(tcp);
        w.write_all(req.as_bytes()).await?;
        w.flush().await?;
        let mut buf: BufReader<_> = BufReader::new(r);
        let mut line = String::new();
        buf.read_line(&mut line).await?;
        Ok(parse_status_code(&line))
    }
}

// 让泛型约束更简洁
use tokio::io::AsyncRead;
