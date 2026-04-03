//! FastCGI 流式响应层：发送请求、读取响应头、组装 HTTP 响应

use sweety_web::{
    body::ResponseBody,
    http::{StatusCode, WebResponse, header::{CONTENT_TYPE, HeaderValue}},
};

use super::proto::{
    FcgiRecord, FCGI_STDOUT, FCGI_STDERR,
    FCGI_BEGIN_REQUEST, FCGI_PARAMS, FCGI_STDIN, FCGI_RESPONDER,
    write_fcgi_header, write_fcgi_record, encode_nv_pair,
    read_fcgi_conn, find_header_end, parse_fcgi_headers,
};

// ─────────────────────────────────────────────
// 内部数据结构
// ─────────────────────────────────────────────

/// FastCGI 响应头解析结果（发送请求 + 读取响应头后返回）
pub(super) struct FcgiParsedHeaders {
    pub(super) status:      u16,
    pub(super) headers:     Vec<(String, String)>,
    pub(super) body_prefix: Vec<u8>,
    pub(super) conn:        crate::handler::fastcgi_pool::FcgiConn,
    pub(super) body_done:   bool,
}

// ─────────────────────────────────────────────
// 请求发送 + 响应头读取
// ─────────────────────────────────────────────

/// 发送 FastCGI 请求并读取响应头（直到 \r\n\r\n），body 留在 conn 里流式读
pub(super) async fn fcgi_send_and_read_headers(
    conn: crate::handler::fastcgi_pool::FcgiConn,
    params: &[(String, String)],
    stdin_body: &[u8],
) -> anyhow::Result<FcgiParsedHeaders> {
    use crate::handler::fastcgi_pool::FcgiConn;

    let rid: u16 = 1;
    let params_est: usize = params.iter().map(|(k, v)| k.len() + v.len() + 8).sum();
    let mut pkt = Vec::with_capacity(8 + 8 + params_est + 8 + stdin_body.len() + 64);
    write_fcgi_header(&mut pkt, FCGI_BEGIN_REQUEST, rid, 8, 0);
    pkt.extend_from_slice(&FCGI_RESPONDER.to_be_bytes());
    pkt.push(1); // FCGI_KEEP_CONN
    pkt.extend_from_slice(&[0u8; 5]);
    {
        let mut body = Vec::with_capacity(params_est);
        for (k, v) in params {
            encode_nv_pair(&mut body, k.as_bytes(), v.as_bytes());
        }
        write_fcgi_record(&mut pkt, FCGI_PARAMS, rid, &body);
        write_fcgi_record(&mut pkt, FCGI_PARAMS, rid, &[]);
    }
    write_fcgi_record(&mut pkt, FCGI_STDIN, rid, stdin_body);
    write_fcgi_record(&mut pkt, FCGI_STDIN, rid, &[]);

    macro_rules! do_send_recv {
        ($stream:expr, $wrap:expr) => {{
            use tokio::io::AsyncWriteExt;
            $stream.write_all(&pkt).await?;
            $stream.flush().await?;
            let (status, headers, body_prefix, body_done) =
                read_headers_from_stream(&mut $stream).await?;
            Ok(FcgiParsedHeaders {
                status,
                headers,
                body_prefix,
                conn: $wrap($stream),
                body_done,
            })
        }};
    }

    match conn {
        FcgiConn::Tcp(mut s) => do_send_recv!(s, FcgiConn::Tcp),
        #[cfg(unix)]
        FcgiConn::Unix(mut s) => do_send_recv!(s, FcgiConn::Unix),
    }
}

/// 从已建立的流读取 STDOUT 直到找到头部分隔符
async fn read_headers_from_stream<S>(stream: &mut S)
    -> anyhow::Result<(u16, Vec<(String, String)>, Vec<u8>, bool)>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut header_buf: Vec<u8> = Vec::with_capacity(4096);
    let mut body_prefix: Vec<u8> = Vec::new();
    let mut body_done = false;

    loop {
        let rec = read_fcgi_from_stream(stream).await?;
        match rec.rec_type {
            t if t == FCGI_STDOUT => {
                if rec.data.is_empty() { body_done = true; break; }
                header_buf.extend_from_slice(&rec.data);
                if let Some((body_start, hdr_text_end)) = find_header_end(&header_buf) {
                    body_prefix = header_buf[body_start..].to_vec();
                    header_buf.truncate(hdr_text_end);
                    break;
                }
            }
            t if t == FCGI_STDERR => {
                if let Ok(s) = std::str::from_utf8(&rec.data) {
                    if !s.trim().is_empty() { tracing::warn!("PHP-FPM stderr: {}", s.trim()); }
                }
            }
            _ => {}
        }
    }

    let header_str = String::from_utf8_lossy(&header_buf);
    let (status, headers) = parse_fcgi_headers(&header_str);
    Ok((status, headers, body_prefix, body_done))
}

/// 从流读一条 FCGI 记录（泛型，与传输类型无关）
async fn read_fcgi_from_stream<S>(stream: &mut S) -> anyhow::Result<FcgiRecord>
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut hdr = [0u8; 8];
    stream.read_exact(&mut hdr).await?;
    let rec_type    = hdr[1];
    let content_len = u16::from_be_bytes([hdr[4], hdr[5]]) as usize;
    let padding_len = hdr[6] as usize;
    let total = content_len + padding_len;
    let mut buf = vec![0u8; total];
    if total > 0 { stream.read_exact(&mut buf).await?; }
    buf.truncate(content_len);
    Ok(FcgiRecord { rec_type, data: buf })
}

// ─────────────────────────────────────────────
// 响应组装
// ─────────────────────────────────────────────

/// FastCGI 响应处理：流式转发，首字节延迟最低
pub(super) async fn build_streaming_response(
    parsed: FcgiParsedHeaders,
    pool: std::sync::Arc<crate::handler::fastcgi_pool::FcgiPool>,
    addr: String,
    fcgi_cache: Option<std::sync::Arc<crate::middleware::proxy_cache::ProxyCache>>,
    cache_key: Option<crate::middleware::proxy_cache::CacheKey>,
) -> WebResponse {
    if parsed.body_done {
        pool.release(&addr, parsed.conn);
        write_fcgi_cache(&fcgi_cache, &cache_key, parsed.status, &parsed.headers, &parsed.body_prefix);
        return make_complete_response(parsed.status, parsed.headers, parsed.body_prefix);
    }

    let mut conn = parsed.conn;
    let mut body = parsed.body_prefix;
    let mut use_stream = false;
    loop {
        let rec = match read_fcgi_conn(&mut conn).await {
            Ok(r) => r,
            Err(_) => break,
        };
        match rec.rec_type {
            t if t == FCGI_STDOUT => {
                if rec.data.is_empty() { break; }
                body.extend_from_slice(&rec.data);
                if body.len() > 4 * 1024 * 1024 { use_stream = true; break; }
            }
            t if t == FCGI_STDERR => {
                if let Ok(s) = std::str::from_utf8(&rec.data) {
                    if !s.trim().is_empty() { tracing::warn!("PHP-FPM stderr: {}", s.trim()); }
                }
            }
            _ => break,
        }
    }

    if use_stream {
        return stream_remaining(body, conn, pool, addr, parsed.status, parsed.headers).await;
    }

    pool.release(&addr, conn);
    write_fcgi_cache(&fcgi_cache, &cache_key, parsed.status, &parsed.headers, &body);
    make_complete_response(parsed.status, parsed.headers, body)
}

fn write_fcgi_cache(
    cache: &Option<std::sync::Arc<crate::middleware::proxy_cache::ProxyCache>>,
    key: &Option<crate::middleware::proxy_cache::CacheKey>,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) {
    if let (Some(cache), Some(key)) = (cache, key) {
        if cache.is_cacheable(status, headers) {
            cache.set(key.clone(), status, headers.to_vec(), bytes::Bytes::copy_from_slice(body));
        }
    }
}

async fn stream_remaining(
    initial: Vec<u8>,
    mut conn: crate::handler::fastcgi_pool::FcgiConn,
    pool: std::sync::Arc<crate::handler::fastcgi_pool::FcgiPool>,
    addr: String,
    status: u16,
    headers: Vec<(String, String)>,
) -> WebResponse {
    use sweety_web::http::header::HeaderName;

    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<bytes::Bytes>>(16);
    tokio::spawn(async move {
        if !initial.is_empty() {
            if tx.send(Ok(bytes::Bytes::from(initial))).await.is_err() { return; }
        }
        loop {
            let rec = match read_fcgi_conn(&mut conn).await {
                Ok(r) => r,
                Err(e) => { let _ = tx.send(Err(e)).await; return; }
            };
            match rec.rec_type {
                t if t == FCGI_STDOUT => {
                    if rec.data.is_empty() { pool.release(&addr, conn); return; }
                    if tx.send(Ok(bytes::Bytes::from(rec.data))).await.is_err() { return; }
                }
                t if t == FCGI_STDERR => {
                    if let Ok(s) = std::str::from_utf8(&rec.data) {
                        if !s.trim().is_empty() { tracing::warn!("PHP-FPM stderr: {}", s.trim()); }
                    }
                }
                _ => { pool.release(&addr, conn); return; }
            }
        }
    });

    let http_status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    let no_body = http_status == StatusCode::NOT_MODIFIED
        || http_status == StatusCode::NO_CONTENT
        || http_status == StatusCode::RESET_CONTENT
        || http_status.is_informational();
    let body_rb = if no_body {
        ResponseBody::none()
    } else {
        let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        ResponseBody::box_stream(stream)
    };
    let mut resp = WebResponse::new(body_rb);
    *resp.status_mut() = http_status;
    for (k, v) in &headers {
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_bytes(v.as_bytes()),
        ) {
            resp.headers_mut().append(name, val);
        }
    }
    if !no_body && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
        resp.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    }
    resp
}

pub(super) fn make_complete_response(status: u16, headers: Vec<(String, String)>, body: Vec<u8>) -> WebResponse {
    use sweety_web::http::header::HeaderName;
    let http_status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    let no_body = http_status == StatusCode::NOT_MODIFIED
        || http_status == StatusCode::NO_CONTENT
        || http_status == StatusCode::RESET_CONTENT
        || http_status.is_informational();
    let body_len = if no_body { 0 } else { body.len() };
    let mut resp = if no_body {
        WebResponse::new(ResponseBody::none())
    } else {
        WebResponse::new(ResponseBody::from(body))
    };
    *resp.status_mut() = http_status;
    for (k, v) in &headers {
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_bytes(v.as_bytes()),
        ) {
            resp.headers_mut().append(name, val);
        }
    }
    if !no_body {
        if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
            resp.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
        }
        if let Ok(v) = HeaderValue::from_str(itoa::Buffer::new().format(body_len)) {
            resp.headers_mut().insert(sweety_web::http::header::CONTENT_LENGTH, v);
        }
    }
    resp
}
