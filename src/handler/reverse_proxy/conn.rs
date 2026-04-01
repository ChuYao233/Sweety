//! HTTP 连接层
//! 负责：与上游建立连接、发送请求、读取响应（支持 chunked/gzip）、健康检查探活

use std::time::{Duration, Instant};

use anyhow::Result;
use futures_util::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing::debug;
use sweety_web::{
    body::{RequestBody, ResponseBody},
    http::{StatusCode, WebResponse, header::{CONTENT_LENGTH, HeaderValue}},
};

use super::error::{IoContext, ProxyError};
use super::pool::{ConnPool, PooledConn};
use super::response::{apply_response_headers, parse_status_code};
use super::tls_client::tls_connect;
use crate::middleware::proxy_cache::{CacheKey, ProxyCache};

/// 向上游转发请求，优先复用连接池里的 idle 连接
/// body 只能消耗一次；外层 retry 循环已做 take，第一次 attempt 传 Some，重试传 None
#[allow(clippy::too_many_arguments)]
pub async fn forward_request(
    pool: &ConnPool,
    upstream_addr: &str,
    method: &str,
    path: &str,
    host: &str,
    use_tls: bool,
    tls_sni: &str,
    tls_insecure: bool,
    extra_headers: &[(String, String)],
    client_ip: &str,
    req_body: RequestBody,
    strip_cookie_secure: bool,
    proxy_cookie_domain: Option<&str>,
    proxy_redirect_from: Option<&str>,
    proxy_redirect_to: Option<&str>,
    sub_filter: &[crate::config::model::SubFilter],
    proxy_cache: Option<(&std::sync::Arc<ProxyCache>, &CacheKey)>,
    client_proto: &str,
    keepalive_requests: u64,
    keepalive_time: u64,
    keepalive_max_idle: usize,
    connect_timeout_secs: u64,
    read_timeout_secs: u64,
    write_timeout_secs: u64,
    // false = 流式透传（不等上游响应体完成即开始转发）
    proxy_buffering: bool,
) -> Result<WebResponse> {
    debug!("转发 {} {} → {} (tls={})", method, path, upstream_addr, use_tls);

    // 从 extra_headers 提取 Content-Length（如果客户端提供了就透传，否则用 chunked）
    let known_content_length: Option<u64> = extra_headers.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse().ok());

    // 检测 Expect: 100-continue（大文件上传协商）
    let has_expect_continue = extra_headers.iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("expect") && v.eq_ignore_ascii_case("100-continue"));

    let key = PooledConn::key(upstream_addr, use_tls);

    // body 用 Option 包装：首次 take() 消费，若 body 未消费则可重试（idle 连接失效场景）
    let mut body_slot = Some(req_body);

    for attempt in 0..2u8 {
        let (conn, created_at, req_count) = if attempt == 0 {
            match pool.acquire(&key) {
                Some((c, ca, rc)) => { debug!("复用 idle 连接: {}", upstream_addr); (c, ca, rc) }
                None => {
                    let c = new_conn(upstream_addr, use_tls, tls_sni, tls_insecure, connect_timeout_secs).await?;
                    (c, Instant::now(), 0u64)
                }
            }
        } else {
            debug!("重试新建连接: {}", upstream_addr);
            let c = new_conn(upstream_addr, use_tls, tls_sni, tls_insecure, connect_timeout_secs).await?;
            (c, Instant::now(), 0u64)
        };

        // take()：消费 body，后续 body_slot = None
        let body = body_slot.take().unwrap_or_default();

        match send_recv_pooled(conn, method, path, host, extra_headers, client_ip,
            body,
            known_content_length, has_expect_continue,
            strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to,
            sub_filter, proxy_cache, client_proto,
            read_timeout_secs, write_timeout_secs, proxy_buffering, attempt == 0,
            pool.clone(), key.clone(), created_at, req_count,
            keepalive_requests, keepalive_time, keepalive_max_idle).await
        {
            Ok((resp, maybe_conn, _body_consumed)) => {
                if let Some(c) = maybe_conn {
                    pool.release(
                        &key, c,
                        created_at, req_count + 1,
                        keepalive_requests, keepalive_time, keepalive_max_idle,
                    );
                }
                return Ok(resp);
            }
            Err((e, body_consumed)) => {
                if attempt == 0 && !body_consumed {
                    // body 还未消费（请求头阶段失败），可安全重试
                    debug!("连接失效（body 未消费），重试: {}", e);
                    continue;
                }
                return Err(e);
            }
        }
    }
    unreachable!()
}

/// 请求体流式写入上游
/// 支持两种模式：
/// 1. 已知 Content-Length：直接写 body 块，上游用 Content-Length 模式接收
/// 2. 未知长度（chunked）：每块加 `<hex>\r\n<data>\r\n`，结束时写 `0\r\n\r\n`
async fn pipe_request_body<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    mut body: RequestBody,
    use_chunked: bool,
    write_timeout: Duration,
) -> std::io::Result<()> {
    loop {
        // 逐块读取（每块加逐包超时）
        let chunk = tokio::time::timeout(write_timeout, body.next()).await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "请求体读取超时"))?;
        match chunk {
            None => break,
            Some(Err(e)) => return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, e.to_string())),
            Some(Ok(bytes)) => {
                if bytes.is_empty() { continue; }
                if use_chunked {
                    // chunked 编码：`<hex_len>\r\n<data>\r\n`
                    let size_line = format!("{:x}\r\n", bytes.len());
                    tokio::time::timeout(write_timeout, async {
                        writer.write_all(size_line.as_bytes()).await?;
                        writer.write_all(&bytes).await?;
                        writer.write_all(b"\r\n").await
                    }).await
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "写上游超时"))?
                    .map_err(|e| std::io::Error::new(e.kind(), format!("写上游: {e}")))?;
                } else {
                    tokio::time::timeout(write_timeout, writer.write_all(&bytes)).await
                    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "写上游超时"))?
                    .map_err(|e| std::io::Error::new(e.kind(), format!("写上游: {e}")))?;
                }
            }
        }
    }
    if use_chunked {
        // 终止 chunk
        tokio::time::timeout(write_timeout, async {
            writer.write_all(b"0\r\n\r\n").await?;
            writer.flush().await
        }).await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "写上游超时"))?
        .map_err(|e| std::io::Error::new(e.kind(), format!("flush 上游: {e}")))?;
    } else {
        tokio::time::timeout(write_timeout, writer.flush()).await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "flush 超时"))?
        .map_err(|e| std::io::Error::new(e.kind(), format!("flush 上游: {e}")))?;
    }
    Ok(())
}

/// 新建一个上游连接
async fn new_conn(
    upstream_addr: &str,
    use_tls: bool,
    tls_sni: &str,
    tls_insecure: bool,
    connect_timeout_secs: u64,
) -> Result<PooledConn> {
    let timeout = if connect_timeout_secs > 0 { connect_timeout_secs } else { 10 };
    let tcp = tokio::time::timeout(
        Duration::from_secs(timeout),
        TcpStream::connect(upstream_addr),
    ).await
    .map_err(|_| ProxyError::ConnTimeout {
        addr: upstream_addr.to_string(),
        timeout_secs: timeout,
    })
    .and_then(|r| r.map_err(|e| ProxyError::from_io(upstream_addr, e, IoContext::Connect)))?;
    // TCP_NODELAY：禁用 Nagle，确保请求头和 body 立即发出，降低代理延迟
    let _ = tcp.set_nodelay(true);

    if use_tls {
        let tls = tls_connect(tcp, tls_sni, tls_insecure).await?;
        Ok(PooledConn::Tls(Box::new(tls)))
    } else {
        Ok(PooledConn::Tcp(tcp))
    }
}

/// HTTP/1.1 请求发送 + 响应读取（支持 chunked 流式请求体、100-Continue、逐包超时）
///
/// 返回：
/// - `Ok((响应, Option<连接>, body_consumed))`
/// - `Err((error, body_consumed))`
///
/// `body_consumed = true` 表示请求体已开始发送，不可重试。
#[allow(clippy::too_many_arguments)]
async fn send_recv_pooled(
    conn: PooledConn,
    method: &str,
    path: &str,
    host: &str,
    extra_headers: &[(String, String)],
    client_ip: &str,
    req_body: RequestBody,
    known_content_length: Option<u64>,
    has_expect_continue: bool,
    strip_cookie_secure: bool,
    proxy_cookie_domain: Option<&str>,
    proxy_redirect_from: Option<&str>,
    proxy_redirect_to: Option<&str>,
    sub_filter: &[crate::config::model::SubFilter],
    proxy_cache: Option<(&std::sync::Arc<ProxyCache>, &CacheKey)>,
    client_proto: &str,
    read_timeout_secs: u64,
    write_timeout_secs: u64,
    proxy_buffering: bool,
    // 是否允许在发送请求头失败时回退（第一次尝试 idle 连接时为 true）
    allow_retry_on_header_fail: bool,
    // 流式路径读完后还连接用的参数
    stream_pool: ConnPool,
    stream_key: String,
    stream_created_at: Instant,
    stream_req_count: u64,
    stream_ka_requests: u64,
    stream_ka_time: u64,
    stream_ka_max_idle: usize,
) -> Result<(WebResponse, Option<PooledConn>, bool), (anyhow::Error, bool)> {
    let read_timeout  = Duration::from_secs(if read_timeout_secs  > 0 { read_timeout_secs  } else { 60 });
    let write_timeout = Duration::from_secs(if write_timeout_secs > 0 { write_timeout_secs } else { 60 });

    // 判断是否需要发送请求体（GET/HEAD/OPTIONS 等通常无 body）
    let has_body = matches!(method, "POST" | "PUT" | "PATCH" | "DELETE");
    // 未知长度时用 chunked 编码，已知长度时直接写 Content-Length
    let use_chunked = has_body && known_content_length.is_none();

    // ── 构造请求头 ──────────────────────────────────────────────────────────
    let mut req = String::with_capacity(
        method.len() + path.len() + host.len() + extra_headers.len() * 32 + 128
    );
    req.push_str(method); req.push(' '); req.push_str(path);
    req.push_str(" HTTP/1.1\r\nHost: "); req.push_str(host); req.push_str("\r\n");
    for (k, v) in extra_headers {
        // content-length / transfer-encoding / expect 由下方统一设置
        if k.eq_ignore_ascii_case("content-length")
            || k.eq_ignore_ascii_case("transfer-encoding")
            || k.eq_ignore_ascii_case("expect") {
            continue;
        }
        req.push_str(k); req.push_str(": "); req.push_str(v); req.push_str("\r\n");
    }
    req.push_str("X-Real-IP: "); req.push_str(client_ip); req.push_str("\r\n");
    req.push_str("X-Forwarded-For: "); req.push_str(client_ip); req.push_str("\r\n");
    req.push_str("X-Forwarded-Proto: "); req.push_str(client_proto); req.push_str("\r\nConnection: keep-alive\r\n");
    if has_body {
        if use_chunked {
            req.push_str("Transfer-Encoding: chunked\r\n");
        } else if let Some(len) = known_content_length {
            req.push_str("Content-Length: ");
            req.push_str(itoa::Buffer::new().format(len));
            req.push_str("\r\n");
        }
        // 100-Continue 协商：只有 has_body 时才有意义
        if has_expect_continue {
            req.push_str("Expect: 100-continue\r\n");
        }
    }
    req.push_str("\r\n");

    debug!("→ {} {} Host:{} chunked={} expect_continue={}", method, path, host, use_chunked, has_expect_continue);

    let mut conn = conn;

    // ── 发送请求头 ──────────────────────────────────────────────────────────
    let header_send_result = tokio::time::timeout(write_timeout, async {
        conn.write_all(req.as_bytes()).await?;
        conn.flush().await
    }).await;
    match header_send_result {
        Err(_) | Ok(Err(_)) if allow_retry_on_header_fail => {
            // 请求头发送失败，body 未消费，可重试
            let e = anyhow::anyhow!(ProxyError::WriteTimeout { addr: String::new(), timeout_secs: write_timeout.as_secs() });
            return Err((e, false));
        }
        Err(_) => return Err((anyhow::anyhow!(ProxyError::WriteTimeout { addr: String::new(), timeout_secs: write_timeout.as_secs() }), false)),
        Ok(Err(e)) => return Err((ProxyError::from_io("", e, IoContext::Write).into(), false)),
        Ok(Ok(())) => {}
    }

    // ── 发送请求体 + 处理 100-Continue（RFC 7231 §5.1.1）────────────────────
    // 正确流程：发头 → BufReader::new(conn) → 读状态行
    //   若 100  → 读掉空行 → pipe body → 读真实状态行
    //   若非100 → has_expect_continue=true 说明上游直接拒绝（417等），不发 body
    //   无 Expect → 先 pipe body → 再读状态行
    let (mut buf, status_code, upstream_http10) = if has_body && has_expect_continue {
        // 发头后等上游决策，BufReader 在此建立，后续所有读取都通过它
        let mut buf = BufReader::new(conn);
        let mut status_line = String::new();
        match tokio::time::timeout(Duration::from_secs(5), buf.read_line(&mut status_line)).await {
            Err(_) | Ok(Err(_)) => return Err((ProxyError::TtfbTimeout { addr: String::new() }.into(), false)),
            Ok(Ok(_)) => {}
        }
        let code = parse_status_code(&status_line);
        if code == 100 {
            // 读掉 "\r\n" 空行
            let mut blank = String::new();
            let _ = buf.read_line(&mut blank).await;
            // 现在发 body
            if let Err(e) = pipe_request_body(buf.get_mut(), req_body, use_chunked, write_timeout).await {
                return Err((e.into(), true));
            }
            // 读真实状态行
            status_line.clear();
            match tokio::time::timeout(read_timeout, buf.read_line(&mut status_line)).await {
                Err(_) => return Err((ProxyError::TtfbTimeout { addr: String::new() }.into(), true)),
                Ok(Err(e)) => return Err((ProxyError::from_io("", e, IoContext::Read).into(), true)),
                Ok(Ok(_)) => {}
            }
            let real_code = parse_status_code(&status_line);
            let http10 = status_line.starts_with("HTTP/1.0");
            tracing::debug!("上游 100→{} ← {} {}", real_code, method, path);
            (buf, real_code, http10)
        } else {
            // 上游直接拒绝（417 等），不发 body
            let http10 = status_line.starts_with("HTTP/1.0");
            tracing::debug!("上游拒绝 {} ← {} {} (expect_continue)", code, method, path);
            (buf, code, http10)
        }
    } else {
        // 无 Expect：先 pipe body（若有），再建 BufReader 读响应
        if has_body {
            if let Err(e) = pipe_request_body(&mut conn, req_body, use_chunked, write_timeout).await {
                return Err((e.into(), true));
            }
        }
        let mut buf = BufReader::new(conn);
        let mut status_line = String::new();
        match tokio::time::timeout(read_timeout, buf.read_line(&mut status_line)).await {
            Err(_) => return Err((ProxyError::TtfbTimeout { addr: String::new() }.into(), has_body)),
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof && !status_line.is_empty() => {}
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionReset
                       || e.kind() == std::io::ErrorKind::BrokenPipe => {
                return Err((ProxyError::ConnReset { addr: String::new() }.into(), has_body));
            }
            Ok(Err(e)) => return Err((ProxyError::from_io("", e, IoContext::Read).into(), has_body)),
            Ok(Ok(_)) => {}
        }
        let code = parse_status_code(&status_line);
        let http10 = status_line.starts_with("HTTP/1.0");
        tracing::debug!("上游 {} ← {} {}", code, method, path);
        (buf, code, http10)
    };

    let mut resp_content_length: Option<usize> = None;
    let mut resp_is_chunked = false;
    // HTTP/1.0 默认 close；同时检测 Trailer 头字段（指示有 chunked trailer）
    let mut resp_conn_close = upstream_http10;
    let mut resp_has_trailer = false;
    let mut response_headers: Vec<(String, String)> = Vec::with_capacity(24);
    // 复用 line 缓冲，避免每行都堆分配
    let mut line = String::with_capacity(128);
    loop {
        line.clear();
        match buf.read_line(&mut line).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err((e.into(), true)),
        }
        let trimmed = line.trim();
        if trimmed.is_empty() { break; }
        if let Some(colon) = trimmed.find(':') {
            let k = trimmed[..colon].trim().to_string();
            let v = trimmed[colon + 1..].trim().to_string();
            // 大小写不敏感比较，零堆分配
            if k.eq_ignore_ascii_case("content-length") { resp_content_length = v.parse().ok(); }
            if k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked") {
                resp_is_chunked = true;
            }
            // Trailer 头：指示 chunked body 后有 trailer 头字段（RFC 7230）
            if k.eq_ignore_ascii_case("trailer") {
                resp_has_trailer = true;
            }
            // 上游要求关闭连接时不归还
            if k.eq_ignore_ascii_case("connection") && v.to_ascii_lowercase().contains("close") {
                resp_conn_close = true;
            }
            response_headers.push((k, v));
        }
    }

    // ── 304/204/205/1xx：HTTP 规范不允许有 body，直接提前返回 ──────────────
    {
        let s = StatusCode::from_u16(status_code).unwrap_or(StatusCode::OK);
        let no_body_status = s == StatusCode::NOT_MODIFIED
            || s == StatusCode::NO_CONTENT
            || s == StatusCode::RESET_CONTENT
            || s.is_informational();
        if no_body_status {
            let maybe_conn = if !resp_conn_close { Some(buf.into_inner()) } else { None };
            let mut resp = WebResponse::new(ResponseBody::none());
            *resp.status_mut() = s;
            apply_response_headers(&mut resp, &response_headers,
                strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to);
            return Ok((resp, maybe_conn, true));
        }
    }

    // ── proxy_buffering = false：流式透传响应体 ─────────────────────────────
    // 不把响应体读进内存，直接用 bounded channel stream 透传给客户端。
    // 适用场景：大文件下载、SSE、长轮询、streaming API。
    // 限制：sub_filter / proxy_cache / URL 替换在流式路径下不生效（需 buffering=true）。
    if !proxy_buffering && sub_filter.is_empty() && proxy_cache.is_none() {
        let http_status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::OK);
        let no_body = http_status == StatusCode::NOT_MODIFIED
            || http_status == StatusCode::NO_CONTENT
            || http_status == StatusCode::RESET_CONTENT
            || http_status.is_informational();

        if no_body {
            // no_body 但有连接可复用（上游 keep-alive）
            let maybe_conn = if !resp_conn_close { Some(buf.into_inner()) } else { None };
            let mut resp = WebResponse::new(ResponseBody::none());
            *resp.status_mut() = http_status;
            apply_response_headers(&mut resp, &response_headers,
                strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to);
            return Ok((resp, maybe_conn, true));
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<bytes::Bytes>>(4);
        let per_chunk_timeout = read_timeout;
        // 上游 Connection: close 时不归还连接
        let can_reuse = !resp_conn_close;

        if resp_is_chunked {
            // chunked 上游：解码后用 BytesMut.freeze() 零拷贝转 Bytes
            // 读完整个 chunked 流后把连接归还到池
            tokio::spawn(async move {
                let mut reader = buf;
                let mut size_line = String::new();
                let mut ok = true;
                'outer: loop {
                    size_line.clear();
                    match tokio::time::timeout(per_chunk_timeout, reader.read_line(&mut size_line)).await {
                        Ok(Ok(0)) | Ok(Err(_)) | Err(_) => { ok = false; break; }
                        Ok(Ok(_)) => {}
                    }
                    let size_str = size_line.trim().split(';').next().unwrap_or("0");
                    let chunk_size = usize::from_str_radix(size_str, 16).unwrap_or(0);
                    if chunk_size == 0 {
                        // 0-chunk: 读完结束行
                        let mut trailer = String::new();
                        let _ = reader.read_line(&mut trailer).await;
                        break;
                    }
                    let mut chunk_buf = bytes::BytesMut::with_capacity(chunk_size);
                    chunk_buf.resize(chunk_size, 0);
                    let mut offset = 0;
                    while offset < chunk_size {
                        match tokio::time::timeout(per_chunk_timeout, reader.read(&mut chunk_buf[offset..])).await {
                            Ok(Ok(0)) => { ok = false; break 'outer; }
                            Ok(Ok(n)) => offset += n,
                            Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => { ok = false; break 'outer; }
                            Ok(Err(e)) => { let _ = tx.send(Err(e)).await; ok = false; break 'outer; }
                            Err(_) => {
                                let e = std::io::Error::new(std::io::ErrorKind::TimedOut, "上游响应体读取超时");
                                let _ = tx.send(Err(e)).await;
                                ok = false; break 'outer;
                            }
                        }
                    }
                    chunk_buf.truncate(offset);
                    if tx.send(Ok(chunk_buf.freeze())).await.is_err() { ok = false; break; }
                    let mut crlf = [0u8; 2];
                    let _ = reader.read_exact(&mut crlf).await;
                }
                // ok 且上游未要求关闭：归还连接到池
                if ok && can_reuse {
                    stream_pool.release(
                        &stream_key, reader.into_inner(),
                        stream_created_at, stream_req_count + 1,
                        stream_ka_requests, stream_ka_time, stream_ka_max_idle,
                    );
                }
            });
        } else {
            // Content-Length 或 EOF 上游
            let stream_len = resp_content_length.unwrap_or(usize::MAX);
            let is_eof_mode = resp_content_length.is_none();
            let chunk_size = crate::handler::sendfile::STREAM_CHUNK;
            tokio::spawn(async move {
                let mut reader = buf;
                let mut remaining = stream_len;
                let mut heap = bytes::BytesMut::with_capacity(chunk_size);
                let mut ok = true;
                loop {
                    if remaining == 0 { break; }
                    let to_read = remaining.min(chunk_size);
                    heap.resize(to_read, 0);
                    match tokio::time::timeout(per_chunk_timeout, reader.read(&mut heap[..to_read])).await {
                        Ok(Ok(0)) => break,
                        Ok(Ok(n)) => {
                            remaining = remaining.saturating_sub(n);
                            if tx.send(Ok(heap.split_to(n).freeze())).await.is_err() { ok = false; break; }
                        }
                        Ok(Err(e)) => { let _ = tx.send(Err(e)).await; ok = false; break; }
                        Err(_) => {
                            let e = std::io::Error::new(std::io::ErrorKind::TimedOut, "上游响应体读取超时");
                            let _ = tx.send(Err(e)).await;
                            ok = false; break;
                        }
                    }
                }
                // Content-Length 模式 + 无错误 + 上游未要求关闭：归还连接到池
                // EOF 模式（is_eof_mode）上游自动关连接，不归还
                if ok && !is_eof_mode && can_reuse {
                    stream_pool.release(
                        &stream_key, reader.into_inner(),
                        stream_created_at, stream_req_count + 1,
                        stream_ka_requests, stream_ka_time, stream_ka_max_idle,
                    );
                }
            });
        }

        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let body = ResponseBody::box_stream(stream);
        let mut resp = WebResponse::new(body);
        *resp.status_mut() = http_status;
        apply_response_headers(&mut resp, &response_headers,
            strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to);
        // chunked 路径：移除上游的 Transfer-Encoding 头，由框架重新决定编码方式
        // Content-Length 路径：保留 Content-Length 告知框架用 length 模式而非 chunked
        if resp_is_chunked {
            resp.headers_mut().remove(sweety_web::http::header::TRANSFER_ENCODING);
        } else if let Some(len) = resp_content_length {
            if let Ok(v) = HeaderValue::from_str(itoa::Buffer::new().format(len)) {
                resp.headers_mut().insert(CONTENT_LENGTH, v);
            }
        }
        return Ok((resp, None, true)); // 连接已 move 进 task，不归还池
    }

    // ── 读取响应体（支持 chunked / Content-Length / EOF）───────────────────
    let (body_bytes, trailer_headers) = if resp_is_chunked {
        let (b, t) = read_chunked_body(&mut buf, resp_has_trailer).await.map_err(|e| (e, true))?;
        (b, t)
    } else if let Some(len) = resp_content_length {
        (read_exact_body(&mut buf, len).await.map_err(|e| (e, true))?, Vec::new())
    } else {
        resp_conn_close = true; // EOF 模式：读完即关
        let mut b = Vec::new();
        let _ = buf.read_to_end(&mut b).await;
        (b, Vec::new())
    };

    let maybe_conn = if !resp_conn_close { Some(buf.into_inner()) } else { drop(buf); None };

    // ── URL 替换（仅文本类型）────────────────────────────────────
    let body_after_url = rewrite_body_urls(body_bytes, &response_headers, proxy_redirect_from, proxy_redirect_to);
    // ── sub_filter 替换（在 URL 替换之后）───────────────────────────
    let final_body = super::response::apply_sub_filter(body_after_url, &response_headers, sub_filter);

    // ── proxy_cache 写入（在 body 完整且未被消耗之前）────────────
    if let Some((cache, cache_key)) = proxy_cache {
        if cache.is_cacheable(status_code, &response_headers) {
            let body_clone = bytes::Bytes::from(final_body.clone());
            let headers_clone = response_headers.clone();
            let cache_clone = cache.clone();
            let key_clone = cache_key.clone();
            tokio::spawn(async move {
                cache_clone.set(key_clone, status_code, headers_clone, body_clone);
            });
        }
    }

    let http_status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::OK);
    let no_body = http_status == StatusCode::NOT_MODIFIED
        || http_status == StatusCode::NO_CONTENT
        || http_status == StatusCode::RESET_CONTENT
        || http_status.is_informational();
    let body_len = if no_body { 0 } else { final_body.len() };
    let mut resp = if no_body {
        WebResponse::new(ResponseBody::none())
    } else {
        WebResponse::new(ResponseBody::from(final_body))
    };
    *resp.status_mut() = http_status;
    apply_response_headers(&mut resp, &response_headers,
        strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to);
    // chunked trailer 头透传给客户端（RFC 7230 §4.1.2）
    append_trailer_headers(&mut resp, &trailer_headers);
    if !no_body {
        if let Ok(v) = HeaderValue::from_str(itoa::Buffer::new().format(body_len)) {
            resp.headers_mut().insert(CONTENT_LENGTH, v);
        }
    }

    Ok((resp, maybe_conn, true))
}

// ─────────────────────────────────────────────
// 响应体读取辅助函数
// ─────────────────────────────────────────────

/// 读取固定长度响应体（循环读取，正确处理 TLS UnexpectedEof）
async fn read_exact_body<R>(buf: &mut BufReader<R>, len: usize) -> Result<Vec<u8>>
where R: AsyncRead + Unpin {
    let mut b = Vec::with_capacity(len);
    // 64KB 栈 buf：减少 syscall 次数，对标 Nginx proxy_buffer_size 64k
    let mut tmp = [0u8; 65536];
    let mut remaining = len;
    loop {
        if remaining == 0 { break; }
        match buf.read(&mut tmp[..remaining.min(65536)]).await {
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
/// 格式：`<hex_size>\r\n<data>\r\n` ... `0\r\n\r\n[trailer-headers]\r\n`
///
/// `collect_trailers = true` 时收集 0-chunk 后面的 trailer 头（RFC 7230 §4.1.2），
/// 返回 `(body_bytes, trailer_headers)`。
pub async fn read_chunked_body<R>(
    buf: &mut BufReader<R>,
    collect_trailers: bool,
) -> Result<(Vec<u8>, Vec<(String, String)>)>
where R: AsyncRead + Unpin {
    let mut body = Vec::new();
    let mut trailers: Vec<(String, String)> = Vec::new();
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
            // 0-chunk 后面是可选的 trailer 头（RFC 7230 §4.1.2）
            if collect_trailers {
                let mut tline = String::new();
                loop {
                    tline.clear();
                    match buf.read_line(&mut tline).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let trimmed = tline.trim();
                    if trimmed.is_empty() { break; } // 空行表示 trailer 结束
                    if let Some(colon) = trimmed.find(':') {
                        let k = trimmed[..colon].trim().to_string();
                        let v = trimmed[colon + 1..].trim().to_string();
                        trailers.push((k, v));
                    }
                }
            } else {
                // 无 trailer：只读掉 CRLF
                let mut skip = String::new();
                let _ = buf.read_line(&mut skip).await;
            }
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
    Ok((body, trailers))
}

/// 将 chunked trailer 头 append 到响应（hop-by-hop 头除外）
/// 等价 Nginx proxy_pass 的 trailer 透传行为
fn append_trailer_headers(resp: &mut WebResponse, trailers: &[(String, String)]) {
    use sweety_web::http::header::HeaderName;
    for (k, v) in trailers {
        if super::response::is_hop_by_hop(k) { continue; }
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            resp.headers_mut().append(name, val);
        }
    }
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
        k.eq_ignore_ascii_case("content-type") && (
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
