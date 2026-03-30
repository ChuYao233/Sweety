//! FastCGI / PHP 处理器
//!
//! # 修复内容（对比旧版本）
//! - **POST/PUT 请求体**：正确通过 STDIN 传递给 PHP-FPM
//! - **全量请求头转发**：所有 HTTP_* 变量（Cookie、Accept、Authorization 等）
//! - **SCRIPT_FILENAME 正确解析**：支持 rewrite 后的 PATH_INFO 分离
//! - **全量响应头转发**：Set-Cookie、Location、X-Powered-By 等全部透传
//! - **CONTENT_TYPE / CONTENT_LENGTH**：POST 请求必须参数
//! - **SERVER_PORT / HTTPS 变量**
//! - 参照 RFC 3875 (CGI) 和 FastCGI 1.0 规范

use xitca_web::{
    body::ResponseBody,
    http::{StatusCode, WebResponse, header::{CONTENT_TYPE, HeaderValue}},
    WebContext,
};

use crate::config::model::LocationConfig;
use crate::dispatcher::vhost::SiteInfo;
use crate::server::http::AppState;

// ─────────────────────────────────────────────
// FastCGI 协议常量（RFC 3875 + FastCGI 1.0）
// ─────────────────────────────────────────────

const FCGI_VERSION:       u8  = 1;
const FCGI_BEGIN_REQUEST: u8  = 1;
const FCGI_PARAMS:        u8  = 4;
const FCGI_STDIN:         u8  = 5;
const FCGI_STDOUT:        u8  = 6;
const FCGI_STDERR:        u8  = 7;
const FCGI_RESPONDER:     u16 = 1;
/// FastCGI 单条记录最大内容长度（65535 字节）
const FCGI_MAX_CONTENT:   usize = 65535;

// ─────────────────────────────────────────────
// FastCGI 数据包结构
// ─────────────────────────────────────────────

/// FastCGI 请求头（8 字节固定长度）
fn write_fcgi_header(buf: &mut Vec<u8>, record_type: u8, request_id: u16, content_len: usize, padding: u8) {
    let id = request_id.to_be_bytes();
    let len = (content_len as u16).to_be_bytes();
    buf.extend_from_slice(&[FCGI_VERSION, record_type, id[0], id[1], len[0], len[1], padding, 0]);
}

/// 写入一条完整的 FCGI 记录（自动分片，支持超过 65535 字节的内容）
fn write_fcgi_record(buf: &mut Vec<u8>, record_type: u8, request_id: u16, data: &[u8]) {
    if data.is_empty() {
        write_fcgi_header(buf, record_type, request_id, 0, 0);
        return;
    }
    for chunk in data.chunks(FCGI_MAX_CONTENT) {
        // 对齐到 8 字节边界
        let padding = (8 - (chunk.len() % 8)) % 8;
        write_fcgi_header(buf, record_type, request_id, chunk.len(), padding as u8);
        buf.extend_from_slice(chunk);
        buf.extend_from_slice(&[0u8; 8][..padding]);
    }
}

/// FastCGI name-value 对编码（RFC 3875 §11.1）
fn encode_nv_pair(buf: &mut Vec<u8>, name: &[u8], value: &[u8]) {
    let enc_len = |b: &mut Vec<u8>, n: usize| {
        if n < 128 {
            b.push(n as u8);
        } else {
            b.extend_from_slice(&((n as u32) | 0x80000000u32).to_be_bytes());
        }
    };
    enc_len(buf, name.len());
    enc_len(buf, value.len());
    buf.extend_from_slice(name);
    buf.extend_from_slice(value);
}

// ─────────────────────────────────────────────
// 主处理函数
// ─────────────────────────────────────────────

/// 处理 FastCGI / PHP 请求
pub async fn handle_xitca(
    ctx: &WebContext<'_, AppState>,
    site: &SiteInfo,
    location: &LocationConfig,
) -> WebResponse {
    // ── FastCGI 后端地址 ──────────────────────────────────────────────────
    let fcgi_cfg = site.fastcgi.as_ref();

    // Unix socket 路径（优先）或 TCP host:port
    let addr_mode = if let Some(sock) = fcgi_cfg.and_then(|f| f.socket.as_ref()) {
        FcgiAddr::Unix(sock.to_string_lossy().into_owned())
    } else {
        let host = fcgi_cfg.and_then(|f| f.host.as_deref()).unwrap_or("127.0.0.1");
        let port = fcgi_cfg.and_then(|f| f.port).unwrap_or(9000);
        FcgiAddr::Tcp(format!("{}:{}", host, port))
    };

    // ── 提取请求信息 ──────────────────────────────────────────────────────
    let method   = ctx.req().method().as_str().to_string();
    let uri      = ctx.req().uri();
    let path_qs  = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let req_uri  = path_qs.to_string();
    let path_raw = uri.path();
    let query    = uri.query().unwrap_or("").to_string();
    let peer     = ctx.req().body().socket_addr();
    let peer_ip  = peer.ip().to_string();
    let peer_port= peer.port().to_string();

    let host_hdr = ctx.req().headers()
        .get("host").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let host_name = host_hdr.split(':').next().unwrap_or(&host_hdr).to_string();
    let host_port_str = host_hdr.split(':').nth(1).unwrap_or("80").to_string();

    // ── 站点 root ─────────────────────────────────────────────────────────
    let root = match location.root.as_ref().or(site.root.as_ref()) {
        Some(r) => r.to_string_lossy().into_owned(),
        None => return fcgi_error(StatusCode::INTERNAL_SERVER_ERROR, "FastCGI: 未配置 root 目录"),
    };

    // ── SCRIPT_FILENAME / SCRIPT_NAME / PATH_INFO 解析 ───────────────────
    // 规则（与 Nginx 一致）：
    //   1. 若 path 以 .php 结尾 → SCRIPT_FILENAME = root + path, PATH_INFO = ""
    //   2. 若 path 包含 .php/ → 分割出 script 和 path_info
    //   3. 否则（rewrite 到 index.php）→ SCRIPT_FILENAME = root/index.php
    let (script_name, path_info) = split_script_path(path_raw);
    let script_filename = format!("{}{}", root.trim_end_matches('/'), script_name);

    // ── 读取请求体 ────────────────────────────────────────────────────────
    // 使用 Content-Length 上限（等价 Nginx fastcgi_read_timeout）
    let max_body = {
        let mb = ctx.state().cfg.global.client_max_body_size;
        (mb as u64) * 1024 * 1024
    };
    let content_length = ctx.req().headers()
        .get(xitca_web::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    let content_type = ctx.req().headers()
        .get(xitca_web::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // 读取请求体（与 reverse_proxy 相同方式：ctx.body_borrow_mut() + StreamExt）
    let req_body: Vec<u8> = if matches!(method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE")
        && content_length <= max_body
    {
        use futures_util::StreamExt;
        let mut collected = Vec::with_capacity(content_length as usize);
        let mut body = ctx.body_borrow_mut();
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(b) => collected.extend_from_slice(b.as_ref()),
                Err(_) => break,
            }
        }
        collected
    } else {
        Vec::new()
    };

    // ── 构建 CGI 参数（RFC 3875）────────────────────────────────────────
    let mut params: Vec<(String, String)> = vec![
        ("SCRIPT_FILENAME".into(), script_filename),
        ("SCRIPT_NAME".into(),     script_name.to_string()),
        ("PATH_INFO".into(),       path_info.to_string()),
        ("PATH_TRANSLATED".into(), format!("{}{}", root.trim_end_matches('/'), path_info)),
        ("REQUEST_METHOD".into(),  method),
        ("REQUEST_URI".into(),     req_uri),
        ("QUERY_STRING".into(),    query),
        ("SERVER_SOFTWARE".into(), "Sweety/0.1".into()),
        ("SERVER_PROTOCOL".into(), "HTTP/1.1".into()),
        ("GATEWAY_INTERFACE".into(),"CGI/1.1".into()),
        ("SERVER_NAME".into(),     host_name),
        ("SERVER_PORT".into(),     host_port_str),
        ("REMOTE_ADDR".into(),     peer_ip),
        ("REMOTE_HOST".into(),     peer.ip().to_string()),
        ("REMOTE_PORT".into(),     peer_port),
        ("DOCUMENT_ROOT".into(),   root.clone()),
        // CONTENT_TYPE / CONTENT_LENGTH 是 POST 必须参数
        ("CONTENT_TYPE".into(),    content_type),
        ("CONTENT_LENGTH".into(),  req_body.len().to_string()),
    ];

    // 判断是否 HTTPS（注入 HTTPS=on，WordPress 依赖此变量判断 is_ssl()）
    let is_https = {
        let port: u16 = host_hdr.split(':').nth(1)
            .and_then(|s| s.parse().ok()).unwrap_or(80);
        ctx.state().tls_ports.contains(&port)
    };
    if is_https {
        params.push(("HTTPS".into(), "on".into()));
    }

    // 将所有 HTTP 请求头转换为 HTTP_* 变量（等价 Nginx fastcgi_param HTTP_* ...）
    // Cookie、Authorization、Accept、Referer、User-Agent 等全部透传
    for (name, value) in ctx.req().headers().iter() {
        let key_str = name.as_str();
        // 跳过已经单独处理的头
        if matches!(key_str, "content-type" | "content-length") { continue; }
        if let Ok(val_str) = value.to_str() {
            let http_key = format!("HTTP_{}", key_str.replace('-', "_").to_uppercase());
            params.push((http_key, val_str.to_string()));
        }
    }

    // ── 发送 FastCGI 请求（连接池 + 超时控制）─────────────────────────────
    let params_ref: Vec<(&str, &str)> = params.iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let pool      = ctx.state().fcgi_pool.clone();
    let read_tmo  = std::time::Duration::from_secs(pool.read_timeout_secs);
    let (addr_str, is_unix) = match &addr_mode {
        FcgiAddr::Unix(p) => (p.clone(), true),
        FcgiAddr::Tcp(p)  => (p.clone(), false),
    };

    // 最多重试一次（idle 连接可能已被 PHP-FPM 关闭）
    for attempt in 0u8..2 {
        let conn = match pool.acquire(&addr_str, is_unix).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("FastCGI 连接失败 {}: {}", addr_str, e);
                return fcgi_error(StatusCode::BAD_GATEWAY, &format!("PHP-FPM 连接失败: {}", e));
            }
        };

        // 带读超时发送请求
        let result = tokio::time::timeout(
            read_tmo,
            send_on_conn(conn, &params_ref, &req_body),
        ).await;

        match result {
            Ok(Ok((stdout, maybe_conn))) => {
                // 归还连接到池
                if let Some(c) = maybe_conn {
                    pool.release(&addr_str, c);
                }
                return parse_fcgi_response(stdout);
            }
            Ok(Err(e)) if attempt == 0 => {
                // idle 连接可能已断开，重试新建
                tracing::debug!("FastCGI idle 连接失效，重试: {}", e);
                continue;
            }
            Ok(Err(e)) => {
                tracing::error!("FastCGI 请求失败 {}: {}", addr_str, e);
                return fcgi_error(StatusCode::BAD_GATEWAY, &format!("PHP-FPM 响应失败: {}", e));
            }
            Err(_) => {
                tracing::error!("FastCGI 读超时 {}s {}", pool.read_timeout_secs, addr_str);
                return fcgi_error(StatusCode::GATEWAY_TIMEOUT, "PHP-FPM 响应超时");
            }
        }
    }

    fcgi_error(StatusCode::BAD_GATEWAY, "PHP-FPM 不可用")
}

/// FastCGI 后端地址枚举（Unix socket 或 TCP）
#[derive(Debug)]
enum FcgiAddr {
    Unix(String),
    Tcp(String),
}

/// 分离 SCRIPT_NAME 和 PATH_INFO
///
/// 示例：
/// - `/index.php`        → (`/index.php`, ``)
/// - `/index.php/foo`    → (`/index.php`, `/foo`)
/// - `/wp-admin/`        → (`/index.php`, `/wp-admin/`)（fallback，WordPress rewrite）
/// - `/foo.php/bar/baz`  → (`/foo.php`, `/bar/baz`)
fn split_script_path(path: &str) -> (&str, &str) {
    // 查找 .php 后面紧跟 / 或结束的位置
    if let Some(pos) = path.find(".php") {
        let end = pos + 4; // ".php" 长度
        if end == path.len() || path.as_bytes()[end] == b'/' {
            return (&path[..end], &path[end..]);
        }
    }
    // 没有找到 .php，fallback 到 index.php（WordPress/Laravel 伪静态）
    ("/index.php", path)
}

/// 构造 FastCGI 错误响应
fn fcgi_error(status: StatusCode, msg: &str) -> WebResponse {
    tracing::warn!("FastCGI 错误 {}: {}", status.as_u16(), msg);
    let body = crate::handler::error_page::build_default_html(status.as_u16());
    let mut resp = WebResponse::new(ResponseBody::from(body));
    *resp.status_mut() = status;
    resp.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    resp
}

/// 在给定连接上发送 FastCGI 请求，返回 (stdout, Option<conn>)
/// Option<conn> 为 Some 表示连接可归还到池（PHP-FPM 保持了 keep-alive）
async fn send_on_conn(
    conn: crate::handler::fastcgi_pool::FcgiConn,
    params: &[(&str, &str)],
    stdin_body: &[u8],
) -> anyhow::Result<(Vec<u8>, Option<crate::handler::fastcgi_pool::FcgiConn>)> {
    use crate::handler::fastcgi_pool::FcgiConn;
    match conn {
        FcgiConn::Tcp(stream) => {
            let (r, w) = tokio::io::split(stream);
            let stdout = fcgi_send_recv(r, w, params, stdin_body).await?;
            // TCP 连接不复用（PHP-FPM flags=0 表示关闭连接）
            Ok((stdout, None))
        }
        #[cfg(unix)]
        FcgiConn::Unix(stream) => {
            let (r, w) = tokio::io::split(stream);
            let stdout = fcgi_send_recv(r, w, params, stdin_body).await?;
            Ok((stdout, None))
        }
    }
}

/// FastCGI 协议发送/接收（通用，与传输类型无关）
async fn fcgi_send_recv<R, W>(
    mut reader: R,
    mut writer: W,
    params: &[(&str, &str)],
    stdin_body: &[u8],
) -> anyhow::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rid: u16 = 1;
    let mut pkt = Vec::with_capacity(4096);

    // 1. BEGIN_REQUEST
    write_fcgi_header(&mut pkt, FCGI_BEGIN_REQUEST, rid, 8, 0);
    pkt.extend_from_slice(&FCGI_RESPONDER.to_be_bytes());
    pkt.push(0); // flags = 0（不复用连接）
    pkt.extend_from_slice(&[0u8; 5]);

    // 2. PARAMS（name-value 编码后分片写入）
    {
        let mut body = Vec::new();
        for (k, v) in params {
            encode_nv_pair(&mut body, k.as_bytes(), v.as_bytes());
        }
        write_fcgi_record(&mut pkt, FCGI_PARAMS, rid, &body);
        write_fcgi_record(&mut pkt, FCGI_PARAMS, rid, &[]); // 空记录表示 PARAMS 结束
    }

    // 3. STDIN（请求体，POST/PUT 时非空；分片发送，单片最大 65535 字节）
    write_fcgi_record(&mut pkt, FCGI_STDIN, rid, stdin_body);
    write_fcgi_record(&mut pkt, FCGI_STDIN, rid, &[]); // 空记录表示 STDIN 结束

    writer.write_all(&pkt).await?;
    writer.flush().await?;

    // 4. 读取所有 STDOUT 记录（直到 EOF）
    let mut stdout = Vec::new();
    let mut header = [0u8; 8];

    loop {
        if reader.read_exact(&mut header).await.is_err() {
            break; // EOF 或连接关闭
        }
        let rec_type    = header[1];
        let content_len = u16::from_be_bytes([header[4], header[5]]) as usize;
        let padding_len = header[6] as usize;

        let total = content_len + padding_len;
        let mut data = vec![0u8; total];
        if total > 0 {
            reader.read_exact(&mut data).await?;
        }

        match rec_type {
            t if t == FCGI_STDOUT => stdout.extend_from_slice(&data[..content_len]),
            t if t == FCGI_STDERR => {
                if let Ok(s) = std::str::from_utf8(&data[..content_len]) {
                    tracing::warn!("PHP-FPM stderr: {}", s.trim());
                }
            }
            _ => {}
        }

        // STDOUT 空记录 = PHP 输出结束
        if rec_type == FCGI_STDOUT && content_len == 0 {
            break;
        }
    }

    Ok(stdout)
}

/// 解析 PHP-FPM 输出为 HTTP 响应
///
/// PHP 输出格式：`响应头\r\n\r\n响应体` 或 `响应头\n\n响应体`
/// 所有响应头全量透传给客户端（Set-Cookie、Location、Content-Type 等）
fn parse_fcgi_response(stdout: Vec<u8>) -> WebResponse {
    // 分割头部和 body：支持 \r\n\r\n 和 \n\n
    let (header_part, body_part) = 'split: {
        // 先找 \r\n\r\n
        for i in 0..stdout.len().saturating_sub(3) {
            if stdout[i] == b'\r' && stdout[i+1] == b'\n'
                && stdout[i+2] == b'\r' && stdout[i+3] == b'\n' {
                break 'split (
                    std::str::from_utf8(&stdout[..i]).unwrap_or(""),
                    stdout[i+4..].to_vec(),
                );
            }
        }
        // 再找 \n\n
        for i in 0..stdout.len().saturating_sub(1) {
            if stdout[i] == b'\n' && stdout[i+1] == b'\n' {
                break 'split (
                    std::str::from_utf8(&stdout[..i]).unwrap_or(""),
                    stdout[i+2..].to_vec(),
                );
            }
        }
        // 没有找到头尾分隔，全部当作 body
        ("", stdout.clone())
    };

    // 解析状态码（Status: 200 OK 或 Status: 302 Found）
    let mut status_code: u16 = 200;
    let mut response_headers: Vec<(String, String)> = Vec::new();

    for line in header_part.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        if let Some(rest) = line.strip_prefix("Status:").or_else(|| line.strip_prefix("status:")) {
            if let Some(code_str) = rest.trim().split_whitespace().next() {
                status_code = code_str.parse().unwrap_or(200);
            }
            continue; // Status 不转发给客户端
        }
        // 所有其他头（Content-Type、Set-Cookie、Location、X-Powered-By 等）全量转发
        if let Some(colon) = line.find(':') {
            let name  = line[..colon].trim().to_string();
            let value = line[colon+1..].trim().to_string();
            response_headers.push((name, value));
        }
    }

    let http_status = StatusCode::from_u16(status_code).unwrap_or(StatusCode::OK);
    let body_len    = body_part.len();
    let mut resp    = WebResponse::new(ResponseBody::from(body_part));
    *resp.status_mut() = http_status;

    // 写入所有响应头
    use xitca_web::http::header::HeaderName;
    for (k, v) in &response_headers {
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_str(v),
        ) {
            resp.headers_mut().append(name, val);
        }
    }

    // 若 PHP 没有输出 Content-Type，默认 text/html
    if !response_headers.iter().any(|(k, _)| k.to_lowercase() == "content-type") {
        resp.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    }

    // 设置 Content-Length（PHP 已知输出长度时有利于 keep-alive 复用）
    if let Ok(v) = HeaderValue::from_str(&body_len.to_string()) {
        resp.headers_mut().insert(
            xitca_web::http::header::CONTENT_LENGTH,
            v,
        );
    }

    resp
}
