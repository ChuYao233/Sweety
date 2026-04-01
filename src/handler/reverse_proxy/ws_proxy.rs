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
use tracing::{debug, trace, warn};
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
    /// 握手响应头之后同包携带的上游首帧（避免首包被吞）
    prefetched_upstream: Option<Bytes>,
    /// 从上游读取的缓冲区（80KB BytesMut， freeze() 零拷贝转 Bytes）
    read_buf:       bytes::BytesMut,
    /// 暂存已从 client body 拿到但尚未写完上游的数据
    pending_write:  Option<Bytes>,
    /// client body 是否已结束
    client_done:    bool,
}

// SAFETY: BiDirStream 作为 response body stream 只在单个 tokio task 里被 poll，
// 不会真正跨线程传递。RequestBody 内含 Rc<RefCell<...>> 是 !Send，但此处
// box_stream 的 Send 约束仅为 BoxBody 的类型系统要求，实际不跨线程。
unsafe impl<R: Send, W: Send> Send for BiDirStream<R, W> {}

impl<R, W> BiDirStream<R, W>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    fn new(upstream_read: R, upstream_write: W, client_body: RequestBody, prefetched_upstream: Option<Bytes>) -> Self {
        let mut read_buf = bytes::BytesMut::with_capacity(65536);
        read_buf.resize(65536, 0);
        Self {
            upstream_read,
            upstream_write,
            client_body,
            prefetched_upstream,
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
        if let Some(ref data) = this.pending_write.as_ref() {
            match Pin::new(&mut this.upstream_write).poll_write(cx, data.as_ref()) {
                Poll::Ready(Ok(n)) if n == data.len() => { this.pending_write = None; }
                Poll::Ready(Ok(n)) => { this.pending_write = Some(data.slice(n..)); }
                Poll::Ready(Err(e)) => { trace!("WS A方向 pending_write 写上游失败: {}", e); this.client_done = true; this.pending_write = None; }
                Poll::Pending => {}
            }
        }
        if !this.client_done && this.pending_write.is_none() {
            match Pin::new(&mut this.client_body).poll_next(cx) {
                Poll::Ready(Some(Ok(b))) => {
                    trace!("WS A方向 client→upstream {}B", b.len());
                    match Pin::new(&mut this.upstream_write).poll_write(cx, b.as_ref()) {
                        Poll::Ready(Ok(n)) if n == b.len() => {}
                        Poll::Ready(Ok(n)) => { this.pending_write = Some(b.slice(n..)); }
                        Poll::Ready(Err(e)) => { trace!("WS A方向写上游失败: {}", e); this.client_done = true; }
                        Poll::Pending => { this.pending_write = Some(b); }
                    }
                }
                Poll::Ready(Some(Err(e))) => { trace!("WS A方向 client body 错误: {}", e); this.client_done = true; }
                Poll::Ready(None) => { trace!("WS A方向 client body EOF"); this.client_done = true; }
                Poll::Pending => {}
            }
        }

        // ── 方向 B：upstream → client ───────────────────────────────
        if let Some(chunk) = this.prefetched_upstream.take() {
            trace!("WS B方向 prefetched upstream→client {}B", chunk.len());
            return Poll::Ready(Some(Ok(chunk)));
        }
        this.read_buf.resize(65536, 0);
        let mut rb = TokioReadBuf::new(&mut this.read_buf);
        match Pin::new(&mut this.upstream_read).poll_read(cx, &mut rb) {
            Poll::Ready(Ok(())) => {
                let n = rb.filled().len();
                if n == 0 {
                    trace!("WS B方向 upstream EOF");
                    return Poll::Ready(None);
                }
                trace!("WS B方向 upstream→client {}B", n);
                let chunk = this.read_buf.split_to(n).freeze();
                Poll::Ready(Some(Ok(chunk)))
            }
            // 上游关闭连接（含 TLS close_notify 缺失）：当 EOF，让流优雅结束
            // 若报 Err，h2_handler 会 RST H2 stream，浏览器报 "WebSocket connection failed"
            Poll::Ready(Err(e)) => { trace!("WS B方向 upstream 关闭: {}", e); Poll::Ready(None) }
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
    match do_ws_proxy(
        ctx, upstream_addr, use_tls, tls_sni, tls_insecure,
        extra_headers, client_ip, upstream_host, path,
        strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to,
        is_h2_ws,
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
    is_h2_ws: bool,
) -> Result<WebResponse> {
    debug!("WS 代理: {} (tls={}) path={} host={}", upstream_addr, use_tls, path, upstream_host);

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
    let is_already_upgrade = extra_headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("upgrade"));
    if !is_already_upgrade {
        upgrade_req.push_str("Upgrade: websocket\r\n");
    }
    // H2 WS（RFC 8441）客户端不发 Sec-WebSocket-Key，但上游 HTTP/1.1 WS 服务器必须有此头
    // 若缺失则自动生成随机 key（与 Nginx 对 H2 WS 的处理方式一致）
    let has_ws_key = extra_headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("sec-websocket-key"));
    if !has_ws_key {
        use base64::Engine as _;
        let mut key_bytes = [0u8; 16];
        key_bytes.iter_mut().enumerate().for_each(|(i, b)| {
            // 用连接时间和索引生成伪随机 key（足够唯一，不需要密码学随机）
            let t = (std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos() >> (i % 8)) as u8;
            *b = t ^ ((i as u32).wrapping_mul(0x9e3779b9) as u8);
        });
        let key = base64::engine::general_purpose::STANDARD.encode(key_bytes);
        upgrade_req.push_str("Sec-WebSocket-Key: "); upgrade_req.push_str(&key); upgrade_req.push_str("\r\n");
    }
    // Sec-WebSocket-Version 必须是 13（RFC 6455）
    let has_ws_version = extra_headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("sec-websocket-version"));
    if !has_ws_version {
        upgrade_req.push_str("Sec-WebSocket-Version: 13\r\n");
    }
    upgrade_req.push_str("Connection: Upgrade\r\n\r\n");

    // 根据是否需要 TLS 分支处理
    if use_tls {
        let tls = tls_connect(tcp, tls_sni, tls_insecure).await?;
        ws_handshake_and_relay(
            ctx, tls, upgrade_req, is_h2_ws,
            strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to,
        ).await
    } else {
        ws_handshake_and_relay(
            ctx, tcp, upgrade_req, is_h2_ws,
            strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to,
        ).await
    }
}

/// 完成上游 WS 握手，建立双向转发管道
///
/// - H1 Upgrade：上游返回 101，给客户端返回 101 + Upgrade 头
/// - H2 extended CONNECT（RFC 8441）：上游返回 101，给客户端返回 200
///   （H2 不需要 Upgrade/Connection 头，直接用 DATA 帧双向传输）
async fn ws_handshake_and_relay<IO>(
    ctx: &WebContext<'_, AppState>,
    mut upstream: IO,
    upgrade_req: String,
    is_h2_ws: bool,
    strip_cookie_secure: bool,
    proxy_cookie_domain: Option<&str>,
    proxy_redirect_from: Option<&str>,
    proxy_redirect_to: Option<&str>,
) -> Result<WebResponse>
where
    IO: AsyncRead + AsyncReadExt + AsyncWrite + AsyncWriteExt + Unpin + Send + 'static,
{
    // 发送 Upgrade 请求
    trace!("WS 发送 Upgrade 请求:\n{}", upgrade_req);
    upstream.write_all(upgrade_req.as_bytes()).await
        .map_err(|e| anyhow::anyhow!("发送 Upgrade 请求失败: {}", e))?;
    upstream.flush().await
        .map_err(|e| anyhow::anyhow!("flush Upgrade 请求失败: {}", e))?;
    trace!("WS Upgrade 请求发送完毕，等待上游 101");

    // 读取上游响应头，等待 101
    let (status_code, response_headers, prefetched_upstream) = read_upstream_headers(&mut upstream).await?;

    if status_code != 101 {
        warn!("WS 上游返回非 101: {}", status_code);
        let mut resp = WebResponse::new(ResponseBody::empty());
        *resp.status_mut() = StatusCode::from_u16(status_code).unwrap_or(StatusCode::BAD_GATEWAY);
        return Ok(resp);
    }

    debug!("WS 上游握手成功（101），建立双向管道");

    // take_body_ref：取走 owned RequestBody（H1/H2 均适用）
    // H1：取走后 dispatcher 的 body_reader 仍正常工作（decoder=Upgrade，src.split() 推数据）
    // H2：RecvStream 直接持有，take 后由 BiDirStream 负责 flow control 释放
    let client_body = ctx.take_body_ref();

    // 拆分上游连接，构造双向 stream
    let (upstream_read, upstream_write) = tokio::io::split(upstream);
    let bidir = BiDirStream::new(upstream_read, upstream_write, client_body, prefetched_upstream);

    let mut final_resp = WebResponse::new(ResponseBody::box_stream(bidir));

    if is_h2_ws {
        // H2 extended CONNECT（RFC 8441）：返回 200，无 Upgrade/Connection 头
        // H2 DATA 帧天然双向，框架持续 poll bidir stream 驱动转发
        *final_resp.status_mut() = StatusCode::OK;
    } else {
        // H1 Upgrade：需要验证客户端握手头（Sec-WebSocket-Key 等），返回 101
        let resp_builder = http_ws::handshake(ctx.req().method(), ctx.req().headers())
            .map_err(|e| anyhow::anyhow!("客户端 WS 握手验证失败: {:?}", e))?;
        let http_resp = resp_builder
            .body(())
            .map_err(|e| anyhow::anyhow!("构建 101 响应失败: {}", e))?;
        *final_resp.status_mut() = http_resp.status();
        for (name, value) in http_resp.headers() {
            final_resp.headers_mut().insert(name.clone(), value.clone());
        }
    }

    apply_response_headers(
        &mut final_resp, &response_headers,
        strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to,
    );

    Ok(final_resp)
}

/// 读取上游 HTTP 响应头，返回（状态码，头列表）
async fn read_upstream_headers<IO>(io: &mut IO) -> Result<(u16, Vec<(String, String)>, Option<Bytes>)>
where IO: AsyncReadExt + Unpin {
    let mut buf = vec![0u8; 4096];
    let mut filled = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    loop {
        if filled >= buf.len() { bail!("上游响应头过长"); }
        tokio::select! {
            n = io.read(&mut buf[filled..]) => {
                match n {
                    Ok(0) => {
                        // 上游关闭连接（含 TLS close_notify 缺失）
                        // 如果已读到完整响应头则正常退出，否则报错
                        if buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") { break; }
                        bail!("上游在握手前关闭连接");
                    }
                    Ok(n) => filled += n,
                    Err(e) => {
                        // TLS close_notify 缺失会报 UnexpectedEof，视为 EOF 处理
                        if buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") { break; }
                        bail!("上游握手读取失败: {}", e);
                    }
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                bail!("等待上游 101 超时");
            }
        }
        if buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    let head_end = buf[..filled].windows(4).position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .ok_or_else(|| anyhow::anyhow!("上游响应头解析失败"))?;
    let header_str = std::str::from_utf8(&buf[..head_end]).unwrap_or("");
    let status_code = parse_status_code(header_str);
    let headers: Vec<(String, String)> = header_str.lines()
        .skip(1)
        .filter_map(|line| line.find(':').map(|i| (
            line[..i].trim().to_string(),
            line[i+1..].trim().to_string(),
        )))
        .collect();

    let prefetched_upstream = if head_end < filled {
        Some(Bytes::copy_from_slice(&buf[head_end..filled]))
    } else {
        None
    };

    Ok((status_code, headers, prefetched_upstream))
}

