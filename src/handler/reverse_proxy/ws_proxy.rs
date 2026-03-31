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

use std::{
    io,
    pin::Pin,
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use anyhow::{Result, bail};
use bytes::Bytes;
use futures_util::stream::Stream;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf as TokioReadBuf};
use tokio::net::TcpStream;
use tracing::{debug, warn};
use sweety_web::{
    body::{RequestBody, ResponseBody},
    http::{StatusCode, WebResponse},
    WebContext,
};

use super::response::{apply_response_headers, parse_status_code};
use super::tls_client::tls_connect;
use crate::server::http::AppState;

/// 双向 WebSocket 转发 Stream
///
/// 框架 poll 响应 body 时，同时驱动两个方向：
/// - client → upstream：从 RequestBody 读帧写 upstream write half
/// - upstream → client：从 upstream read half 读帧，输出给框架
///
/// 与 Nginx 单 worker 事件循环等价：一次 poll 同时检查两个 fd 的 ready 状态。
struct BiDirStream<R, W> {
    upstream_read:  R,
    upstream_write: W,
    client_body:    RequestBody,
    /// 从上游读取的缓冲区（80KB BytesMut， freeze() 零拷贝转 Bytes）
    read_buf:       bytes::BytesMut,
    /// 暂存已从 client body 拿到但尚未写完上游的数据
    pending_write:  Option<Bytes>,
    /// client body 是否已结束
    client_done:    bool,
}

impl<R, W> BiDirStream<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    fn new(upstream_read: R, upstream_write: W, client_body: RequestBody) -> Self {
        let mut read_buf = bytes::BytesMut::with_capacity(65536);
        read_buf.resize(65536, 0);
        Self {
            upstream_read,
            upstream_write,
            client_body,
            read_buf,
            pending_write: None,
            client_done: false,
        }
    }
}

impl<R, W> Stream for BiDirStream<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    type Item = Result<Bytes, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Result<Bytes, io::Error>>> {
        let this = self.get_mut();

        // ── 方向 A：client → upstream ──────────────────────────────
        // 先把上次未写完的数据继续写
        if let Some(ref data) = this.pending_write.as_ref() {
            match Pin::new(&mut this.upstream_write).poll_write(cx, data.as_ref()) {
                Poll::Ready(Ok(n)) if n == data.len() => { this.pending_write = None; }
                Poll::Ready(Ok(n)) => { this.pending_write = Some(data.slice(n..)); }
                Poll::Ready(Err(_)) => { this.client_done = true; this.pending_write = None; }
                Poll::Pending => {} // 写缓冲满，稍后继续
            }
        }
        // 从 client body 读下一帧
        if !this.client_done && this.pending_write.is_none() {
            match Pin::new(&mut this.client_body).poll_next(cx) {
                Poll::Ready(Some(Ok(b))) => {
                    // 尝试立即写上游
                    match Pin::new(&mut this.upstream_write).poll_write(cx, b.as_ref()) {
                        Poll::Ready(Ok(n)) if n == b.len() => {}
                        Poll::Ready(Ok(n)) => { this.pending_write = Some(b.slice(n..)); }
                        Poll::Ready(Err(_)) => { this.client_done = true; }
                        Poll::Pending => { this.pending_write = Some(b); }
                    }
                }
                Poll::Ready(Some(Err(_))) | Poll::Ready(None) => { this.client_done = true; }
                Poll::Pending => {}
            }
        }

        // ── 方向 B：upstream → client ───────────────────────────────
        // 读入 BytesMut 后 split_to().freeze() 零拷贝转 Bytes，与 Nginx sendfile 语义相同
        this.read_buf.resize(65536, 0);
        let mut rb = TokioReadBuf::new(&mut this.read_buf);
        match Pin::new(&mut this.upstream_read).poll_read(cx, &mut rb) {
            Poll::Ready(Ok(())) => {
                let n = rb.filled().len();
                if n == 0 {
                    // 上游关闭连接，结束 stream
                    return Poll::Ready(None);
                }
                // freeze() 零拷贝：内部将 BytesMut 的内存所有权转移给 Bytes，不复制
                let chunk = this.read_buf.split_to(n).freeze();
                Poll::Ready(Some(Ok(chunk)))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
            Poll::Pending => Poll::Pending,
        }
    }
}

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
    // H2 extended CONNECT：sweety-web 不支持 RFC 8441 双向流，
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
    // TCP_NODELAY：WebSocket 帧通常很小，禁用 Nagle 降低帧延迟
    let _ = tcp.set_nodelay(true);

    // ── Step 2：构造并发送 HTTP Upgrade 请求 ─────────────────────────────
    // 预分配容量，用 push_str 替代 format! 减少堆分配
    let mut upgrade_req = String::with_capacity(
        path.len() + upstream_host.len() + extra_headers.len() * 32 + 128
    );
    upgrade_req.push_str("GET "); upgrade_req.push_str(path);
    upgrade_req.push_str(" HTTP/1.1\r\nHost: "); upgrade_req.push_str(upstream_host);
    upgrade_req.push_str("\r\n");
    for (k, v) in extra_headers {
        upgrade_req.push_str(k); upgrade_req.push_str(": "); upgrade_req.push_str(v); upgrade_req.push_str("\r\n");
    }
    upgrade_req.push_str("X-Real-IP: "); upgrade_req.push_str(client_ip); upgrade_req.push_str("\r\n");
    upgrade_req.push_str("X-Forwarded-For: "); upgrade_req.push_str(client_ip); upgrade_req.push_str("\r\n");
    // 对标 Nginx: proxy_http_version 1.1 + proxy_set_header Upgrade $http_upgrade
    // 必须显式加上 Upgrade + Connection，上游才能完成 WebSocket 升级握手
    // Sec-WebSocket-Key/Version/Extensions 已通过 extra_headers 透传（mod.rs 保留了）
    let is_already_upgrade = extra_headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("upgrade"));
    if !is_already_upgrade {
        upgrade_req.push_str("Upgrade: websocket\r\n");
    }
    upgrade_req.push_str("Connection: Upgrade\r\n\r\n");

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
///
/// 使用 BiDirStream：框架 poll 响应 body 时同时驱动两个方向，
/// 无需跨线程，与 Nginx 单 worker 事件循环等价。
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
    IO: AsyncRead + AsyncReadExt + AsyncWrite + AsyncWriteExt + Unpin + Send + 'static,
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

    // 构造 101 响应头（需借用 ctx.req()，在 take_body_ref 之前完成）
    let resp_builder = http_ws::handshake(ctx.req().method(), ctx.req().headers())
        .map_err(|e| anyhow::anyhow!("客户端 WS 握手验证失败: {:?}", e))?;
    let http_resp = resp_builder
        .body(())
        .map_err(|e| anyhow::anyhow!("构建 101 响应失败: {}", e))?;

    // take_body_ref：RefCell 内部可变性拿走 owned RequestBody
    let client_body = ctx.take_body_ref();

    // 拆分上游连接，构造双向 stream
    let (upstream_read, upstream_write) = tokio::io::split(upstream);
    let bidir = BiDirStream::new(upstream_read, upstream_write, client_body);

    // 立即返回 101，框架持续 poll bidir stream 驱动双向转发
    let mut final_resp = WebResponse::new(ResponseBody::box_stream(bidir));
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

