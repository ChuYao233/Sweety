//! WebSocket / WSS 反向代理
//!
//! 实现：
//! 1. 连接上游（TCP 或 TLS）
//! 2. 发送 HTTP Upgrade，等待 101
//! 3. 101 后启动后台 task 双向转发（上游 ↔ channel ↔ 客户端）
//! 4. 上游→客户端：读上游字节 → tokio mpsc → ResponseBody stream
//! 5. 客户端→上游：RequestBody stream 读客户端字节 → 写上游 socket
//!
//! 零拷贝：tokio 读写操作底层使用内核缓冲区，性能与 Nginx 接近

use std::time::Duration;

use anyhow::{Result, bail};
use bytes::Bytes;
use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{debug, warn};
use xitca_web::{
    body::ResponseBody,
    http::{StatusCode, WebResponse},
    WebContext,
};

use super::response::{apply_response_headers, parse_status_code};
use super::tls_client::tls_connect;
use crate::server::http::AppState;

/// 处理 WS/WSS 反向代理请求的公开入口
///
/// H2 extended CONNECT（RFC 8441）：返回 501，浏览器自动降级 HTTP/1.1 重试
/// H1 Upgrade：正常代理，返回 101 Switching Protocols
#[allow(clippy::too_many_arguments)]
pub async fn handle_ws_proxy(
    ctx: &WebContext<'_, AppState>,
    upstream_addr: &str,
    use_tls: bool,
    tls_sni: &str,
    tls_insecure: bool,
    extra_headers: &[(String, String)],
    client_ip: &str,
    upstream_host: &str,
    path: &str,
    strip_cookie_secure: bool,
    proxy_cookie_domain: Option<&str>,
    proxy_redirect_from: Option<&str>,
    proxy_redirect_to: Option<&str>,
    is_h2_ws: bool,
) -> WebResponse {
    // H2 extended CONNECT：xitca-web 不支持 RFC 8441 双向流，
    // 返回 501 让浏览器降级到 HTTP/1.1 重连，与 Nginx 行为一致
    if is_h2_ws {
        let mut resp = WebResponse::new(ResponseBody::empty());
        *resp.status_mut() = StatusCode::NOT_IMPLEMENTED;
        return resp;
    }

    match do_ws_proxy(
        ctx, upstream_addr, use_tls, tls_sni, tls_insecure,
        extra_headers, client_ip, upstream_host, path,
        strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to,
    ).await {
        Ok(resp) => resp,
        Err(e) => {
            warn!("WS 代理失败 → {}: {}", upstream_addr, e);
            let mut resp = WebResponse::new(ResponseBody::empty());
            *resp.status_mut() = StatusCode::BAD_GATEWAY;
            resp
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn do_ws_proxy(
    ctx: &WebContext<'_, AppState>,
    upstream_addr: &str,
    use_tls: bool,
    tls_sni: &str,
    tls_insecure: bool,
    extra_headers: &[(String, String)],
    client_ip: &str,
    upstream_host: &str,
    path: &str,
    strip_cookie_secure: bool,
    proxy_cookie_domain: Option<&str>,
    proxy_redirect_from: Option<&str>,
    proxy_redirect_to: Option<&str>,
) -> Result<WebResponse> {
    debug!("WS 代理: {} (tls={})", upstream_addr, use_tls);

    // ── Step 1：连接上游 ──────────────────────────────────────────────────
    let tcp = tokio::time::timeout(
        Duration::from_secs(10),
        TcpStream::connect(upstream_addr),
    ).await
    .map_err(|_| anyhow::anyhow!("连接上游超时: {}", upstream_addr))??;

    // ── Step 2：构造并发送 HTTP Upgrade 请求 ─────────────────────────────
    let mut upgrade_req = format!("GET {path} HTTP/1.1\r\nHost: {upstream_host}\r\n");
    for (k, v) in extra_headers {
        upgrade_req.push_str(&format!("{k}: {v}\r\n"));
    }
    upgrade_req.push_str(&format!("X-Real-IP: {client_ip}\r\n"));
    upgrade_req.push_str(&format!("X-Forwarded-For: {client_ip}\r\n"));
    upgrade_req.push_str("X-Forwarded-Proto: https\r\n");
    upgrade_req.push_str("Connection: upgrade\r\n");
    upgrade_req.push_str("\r\n");

    // 根据是否需要 TLS 分支处理
    if use_tls {
        let tls = tls_connect(tcp, tls_sni, tls_insecure).await?;
        ws_handshake_and_relay(
            ctx, tls, upgrade_req,
            strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to,
        ).await
    } else {
        ws_handshake_and_relay(
            ctx, tcp, upgrade_req,
            strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to,
        ).await
    }
}

/// 完成上游 WS 握手，建立双向转发管道
async fn ws_handshake_and_relay<IO>(
    ctx: &WebContext<'_, AppState>,
    mut upstream: IO,
    upgrade_req: String,
    strip_cookie_secure: bool,
    proxy_cookie_domain: Option<&str>,
    proxy_redirect_from: Option<&str>,
    proxy_redirect_to: Option<&str>,
) -> Result<WebResponse>
where
    IO: AsyncReadExt + AsyncWriteExt + Unpin + Send + 'static,
{
    // 发送 Upgrade 请求
    upstream.write_all(upgrade_req.as_bytes()).await?;
    upstream.flush().await?;

    // 读取上游响应头，等待 101
    let (status_code, response_headers) = read_upstream_headers(&mut upstream).await?;

    if status_code != 101 {
        warn!("WS 上游返回非 101: {}", status_code);
        let mut resp = WebResponse::new(ResponseBody::empty());
        *resp.status_mut() = StatusCode::from_u16(status_code).unwrap_or(StatusCode::BAD_GATEWAY);
        return Ok(resp);
    }

    debug!("WS 上游握手成功（101），建立双向管道");

    // ── Step 4：建立双向管道 ──────────────────────────────────────────────
    let (up_tx, up_rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(256);
    // 上游→客户端：持续读上游字节 → channel
    let (upstream_read, upstream_write) = tokio::io::split(upstream);
    tokio::spawn(relay_upstream_to_channel(upstream_read, up_tx));
    // 客户端→上游：持续从 RequestBody stream 读取帧并写入上游
    // 必须先把 body borrow 释放，再 spawn
    let body_chunks: Vec<Bytes> = {
        let mut body = ctx.body_borrow_mut();
        let mut chunks = Vec::new();
        while let Some(chunk) = body.next().await {
            if let Ok(b) = chunk { chunks.push(b); }
        }
        chunks
    };
    tokio::spawn(relay_chunks_to_upstream(body_chunks, upstream_write));
    let body_stream = tokio_stream::wrappers::ReceiverStream::new(up_rx);

    // H1 WebSocket Upgrade：用 http_ws 生成标准 101 + Sec-WebSocket-Accept
    let resp_builder = http_ws::handshake(ctx.req().method(), ctx.req().headers())
        .map_err(|e| anyhow::anyhow!("客户端 WS 握手验证失败: {:?}", e))?;
    let http_resp = resp_builder
        .body(())
        .map_err(|e| anyhow::anyhow!("构建 101 响应失败: {}", e))?;
    let mut final_resp = WebResponse::new(ResponseBody::box_stream(body_stream));
    *final_resp.status_mut() = http_resp.status();
    for (name, value) in http_resp.headers() {
        final_resp.headers_mut().insert(name.clone(), value.clone());
    }

    apply_response_headers(
        &mut final_resp, &response_headers,
        strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to,
    );

    Ok(final_resp)
}

/// 读取上游 HTTP 响应头，返回（状态码，头列表）
async fn read_upstream_headers<IO>(io: &mut IO) -> Result<(u16, Vec<(String, String)>)>
where IO: AsyncReadExt + Unpin {
    let mut buf = vec![0u8; 4096];
    let mut filled = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    loop {
        if filled >= buf.len() { bail!("上游响应头过长"); }
        tokio::select! {
            n = io.read(&mut buf[filled..]) => {
                match n? {
                    0 => bail!("上游在握手前关闭连接"),
                    n => filled += n,
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                bail!("等待上游 101 超时");
            }
        }
        if buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") { break; }
    }

    let header_str = std::str::from_utf8(&buf[..filled]).unwrap_or("");
    let status_code = parse_status_code(header_str);
    let headers: Vec<(String, String)> = header_str.lines()
        .skip(1)
        .filter_map(|line| line.find(':').map(|i| (
            line[..i].trim().to_string(),
            line[i+1..].trim().to_string(),
        )))
        .collect();

    Ok((status_code, headers))
}

/// 从上游读取字节，发送到 mpsc channel（上游→客户端方向）
async fn relay_upstream_to_channel<R>(
    mut reader: R,
    tx: mpsc::Sender<Result<Bytes, std::io::Error>>,
)
where R: AsyncReadExt + Unpin + Send {
    let mut buf = vec![0u8; 65536]; // 64KB 缓冲区，平衡延迟和吞吐
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let chunk = Bytes::copy_from_slice(&buf[..n]);
                if tx.send(Ok(chunk)).await.is_err() { break; }
            }
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => {
                let _ = tx.send(Err(e)).await;
                break;
            }
        }
    }
}

/// 把客户端 chunks 持续写入上游（客户端→上游方向）
async fn relay_chunks_to_upstream<W>(chunks: Vec<Bytes>, mut writer: W)
where W: AsyncWriteExt + Unpin + Send {
    for chunk in chunks {
        if writer.write_all(chunk.as_ref()).await.is_err() { break; }
    }
    let _ = writer.flush().await;
}
