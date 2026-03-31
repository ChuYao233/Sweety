//! HTTP 连接层
//! 负责：与上游建立连接、发送请求、读取响应（支持 chunked/gzip）、健康检查探活

use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tracing::debug;
use sweety_web::{
    body::ResponseBody,
    http::{StatusCode, WebResponse, header::{CONTENT_LENGTH, HeaderValue}},
};

use super::error::{IoContext, ProxyError};
use super::pool::{ConnPool, PooledConn};
use super::response::{apply_response_headers, parse_status_code};
use super::tls_client::tls_connect;
use crate::middleware::proxy_cache::{CacheKey, ProxyCache};

/// 向上游转发请求，优先复用连接池里的 idle 连接
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
    body: &[u8],
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
    // 超时控制（0 = 用默认）
    connect_timeout_secs: u64,
    read_timeout_secs: u64,
    write_timeout_secs: u64,
    // false = 流式透传（不等上游响应体完成即开始转发）
    proxy_buffering: bool,
) -> Result<WebResponse> {
    debug!("转发 {} {} → {} (tls={}, body={}B)",
        method, path, upstream_addr, use_tls, body.len());

    let key = PooledConn::key(upstream_addr, use_tls);

    // 尝试从池取 idle 连接，最多重试一次（防止服务端关闭了 idle 连接）
    for attempt in 0..2u8 {
        // (conn, created_at, request_count)
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

        match send_recv_pooled(conn, method, path, host, extra_headers, client_ip, body,
            strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to,
            sub_filter, proxy_cache, client_proto,
            read_timeout_secs, write_timeout_secs, proxy_buffering).await
        {
            Ok((resp, maybe_conn)) => {
                if let Some(c) = maybe_conn {
                    pool.release(
                        &key, c,
                        created_at, req_count + 1,
                        keepalive_requests, keepalive_time, keepalive_max_idle,
                    );
                }
                return Ok(resp);
            }
            Err(e) if attempt == 0 => {
                debug!("连接失效，重试: {}", e);
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    unreachable!()
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

/// HTTP/1.1 请求发送 + 响应读取（支持 chunked、Content-Length、gzip 透传）
/// 返回：(响应, Option<连接>)，连接为 Some 表示可归还到池（上游保持 keep-alive）
#[allow(clippy::too_many_arguments)]
async fn send_recv_pooled(
    conn: PooledConn,
    method: &str,
    path: &str,
    host: &str,
    extra_headers: &[(String, String)],
    client_ip: &str,
    req_body: &[u8],
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
) -> Result<(WebResponse, Option<PooledConn>)> {
    let read_timeout  = Duration::from_secs(if read_timeout_secs  > 0 { read_timeout_secs  } else { 60 });
    let write_timeout = Duration::from_secs(if write_timeout_secs > 0 { write_timeout_secs } else { 60 });
    // ── 构造请求头（keep-alive）──────────────────────────────────────────────
    // 预分配容量：请求行 + host + 过滤后的头 + 4 个固定头 + \r\n
    let mut req = String::with_capacity(
        method.len() + path.len() + host.len() + extra_headers.len() * 32 + 128
    );
    req.push_str(method); req.push(' '); req.push_str(path);
    req.push_str(" HTTP/1.1\r\nHost: "); req.push_str(host); req.push_str("\r\n");
    for (k, v) in extra_headers {
        // content-length 由下方统一设置，避免与 extra_headers 里的重复
        if k.eq_ignore_ascii_case("content-length") { continue; }
        req.push_str(k); req.push_str(": "); req.push_str(v); req.push_str("\r\n");
    }
    req.push_str("X-Real-IP: "); req.push_str(client_ip); req.push_str("\r\n");
    req.push_str("X-Forwarded-For: "); req.push_str(client_ip); req.push_str("\r\n");
    req.push_str("X-Forwarded-Proto: "); req.push_str(client_proto); req.push_str("\r\nConnection: keep-alive\r\nContent-Length: ");
    req.push_str(itoa::Buffer::new().format(req_body.len())); req.push_str("\r\n\r\n");

    debug!("→ {} {} Host:{} body={}B", method, path, host, req_body.len());

    // HTTP/1.1 串行：先写请求，再读响应
    // BufReader::new(conn) 直接 move conn，后续流式路径可直接 move buf 进 task
    let mut conn = conn;
    // 合并请求头+body 为单次发送，减少 syscall（对标 Nginx writev 行为）
    let send_result = tokio::time::timeout(write_timeout, async {
        if req_body.is_empty() {
            // 无 body：直接写头部
            conn.write_all(req.as_bytes()).await?;
        } else {
            // 有 body：头部+body 一次性写出，内核层面可能合并为单个 TCP 段
            conn.write_all(req.as_bytes()).await?;
            conn.write_all(req_body).await?;
        }
        conn.flush().await
    }).await
    .map_err(|_| ProxyError::WriteTimeout { addr: String::new(), timeout_secs: write_timeout.as_secs() })
    .and_then(|r| r.map_err(|e| ProxyError::from_io("", e, IoContext::Write)))?;
    let _ = send_result;

    // ── 读取响应头（BufReader move conn，不借用）─────────────────────────
    let mut buf = BufReader::new(conn);

    let mut status_line = String::new();
    match tokio::time::timeout(read_timeout, buf.read_line(&mut status_line)).await {
        Err(_) => return Err(ProxyError::TtfbTimeout { addr: String::new() }.into()),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof && !status_line.is_empty() => {}
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionReset
                   || e.kind() == std::io::ErrorKind::BrokenPipe => {
            return Err(ProxyError::ConnReset { addr: String::new() }.into());
        }
        Ok(Err(e)) => return Err(ProxyError::from_io("", e, IoContext::Read).into()),
        Ok(Ok(_)) => {}
    }

    let status_code = parse_status_code(&status_line);
    tracing::debug!("上游 {} ← {} {}", status_code, method, path);

    let mut resp_content_length: Option<usize> = None;
    let mut resp_is_chunked = false;
    let mut resp_conn_close = false;
    let mut response_headers: Vec<(String, String)> = Vec::with_capacity(24);
    // 复用 line 缓冲，避免每行都堆分配
    let mut line = String::with_capacity(128);
    loop {
        line.clear();
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
            // 大小写不敏感比较，零堆分配
            if k.eq_ignore_ascii_case("content-length") { resp_content_length = v.parse().ok(); }
            if k.eq_ignore_ascii_case("transfer-encoding") && v.to_ascii_lowercase().contains("chunked") {
                resp_is_chunked = true;
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
            return Ok((resp, maybe_conn));
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
            let mut resp = WebResponse::new(ResponseBody::none());
            *resp.status_mut() = http_status;
            apply_response_headers(&mut resp, &response_headers,
                strip_cookie_secure, proxy_cookie_domain, proxy_redirect_from, proxy_redirect_to);
            return Ok((resp, None));
        }

        // 用 bounded channel 流式转发：生产者读上游解码后数据，消费者写客户端
        // cap=16：生产者（读上游）和消费者（写客户端）之间缓冲 16 个 chunk
        let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<bytes::Bytes>>(16);

        if resp_is_chunked {
            // chunked 上游：解码后用 BytesMut.freeze() 零拷贝转 Bytes
            tokio::spawn(async move {
                let mut reader = buf;
                let mut size_line = String::new();
                loop {
                    size_line.clear();
                    match reader.read_line(&mut size_line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    let size_str = size_line.trim().split(';').next().unwrap_or("0");
                    let chunk_size = usize::from_str_radix(size_str, 16).unwrap_or(0);
                    if chunk_size == 0 {
                        let mut trailer = String::new();
                        let _ = reader.read_line(&mut trailer).await;
                        break;
                    }
                    // BytesMut 直接读入，freeze() 零拷贝转 Bytes
                    let mut buf = bytes::BytesMut::with_capacity(chunk_size);
                    buf.resize(chunk_size, 0);
                    let mut offset = 0;
                    while offset < chunk_size {
                        match reader.read(&mut buf[offset..]).await {
                            Ok(0) => break,
                            Ok(n) => offset += n,
                            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                            Err(e) => { let _ = tx.send(Err(e)).await; return; }
                        }
                    }
                    buf.truncate(offset);
                    if tx.send(Ok(buf.freeze())).await.is_err() { break; }
                    // 读 chunk 后面的 \r\n
                    let mut crlf = [0u8; 2];
                    let _ = reader.read_exact(&mut crlf).await;
                }
            });
        } else {
            // Content-Length 或 EOF 上游：BytesMut.freeze() 零拷贝透传
            let stream_len = resp_content_length.unwrap_or(usize::MAX);
            let chunk_size = crate::handler::sendfile::STREAM_CHUNK;
            tokio::spawn(async move {
                let mut reader = buf;
                let mut remaining = stream_len;
                let mut heap = bytes::BytesMut::with_capacity(chunk_size);
                loop {
                    if remaining == 0 { break; }
                    let to_read = remaining.min(chunk_size);
                    heap.resize(to_read, 0);
                    match reader.read(&mut heap[..to_read]).await {
                        Ok(0) => break,
                        Ok(n) => {
                            remaining = remaining.saturating_sub(n);
                            // split_to(n).freeze()：将前 n 字节的所有权转移，不复制
                            if tx.send(Ok(heap.split_to(n).freeze())).await.is_err() { break; }
                        }
                        Err(e) => { let _ = tx.send(Err(e)).await; break; }
                    }
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
        return Ok((resp, None)); // 连接已 move 进 task，不归还池
    }

    // ── 读取响应体（支持 chunked / Content-Length / EOF）───────────────────
    let body_bytes = if resp_is_chunked {
        read_chunked_body(&mut buf).await?
    } else if let Some(len) = resp_content_length {
        read_exact_body(&mut buf, len).await?
    } else {
        resp_conn_close = true; // EOF 模式：读完即关
        let mut b = Vec::new();
        let _ = buf.read_to_end(&mut b).await;
        b
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
    if !no_body {
        if let Ok(v) = HeaderValue::from_str(itoa::Buffer::new().format(body_len)) {
            resp.headers_mut().insert(CONTENT_LENGTH, v);
        }
    }

    Ok((resp, maybe_conn))
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
