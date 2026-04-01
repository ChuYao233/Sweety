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

use sweety_web::{
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
///
/// `resolved_script`：由 try_files 解析得到的绝对脚本路径；
/// 若为 None，则回退到从请求路径推断（直接访问 .php 文件时）。
pub async fn handle_sweety(
    ctx: &WebContext<'_, AppState>,
    site: &SiteInfo,
    location: &LocationConfig,
    resolved_script: Option<&std::path::Path>,
) -> WebResponse {
    use crate::middleware::proxy_cache::CacheKey;

    // ── FastCGI 缓存查询 ────────────────────────────────────────────────
    let fcgi_cache = site.fcgi_cache_arc.clone();
    let method_str = ctx.req().method().as_str();
    let req_path   = ctx.req().uri().path_and_query().map(|p| p.as_str()).unwrap_or("/");
    let host_str_owned: String = ctx.req().uri().authority()
        .map(|a| a.as_str().to_string())
        .or_else(|| ctx.req().headers().get("host").and_then(|v| v.to_str().ok()).map(|s| s.to_string()))
        .unwrap_or_default();
    let host_str = host_str_owned.as_str();

    if let Some(cache) = &fcgi_cache {
        // \u76f4\u63a5\u4f20 HeaderMap\uff0c\u8df3\u8fc7\u4e2d\u95f4 Vec \u5806\u5206\u914d
        if cache.should_lookup(method_str, ctx.req().headers()) {
            let key = CacheKey::new(method_str, host_str, req_path);
            if let Some(entry) = cache.get(&key) {
                // 命中：直接返回缓存响应
                use sweety_web::http::header::HeaderName;
                let mut resp = WebResponse::new(
                    sweety_web::body::ResponseBody::from(entry.body.to_vec())
                );
                *resp.status_mut() = StatusCode::from_u16(entry.status).unwrap_or(StatusCode::OK);
                for (k, v) in &entry.headers {
                    if let (Ok(name), Ok(val)) = (
                        HeaderName::from_bytes(k.as_bytes()),
                        HeaderValue::from_bytes(v.as_bytes()),
                    ) {
                        resp.headers_mut().append(name, val);
                    }
                }
                resp.headers_mut().insert(
                    sweety_web::http::header::HeaderName::from_static("x-fastcgi-cache"),
                    HeaderValue::from_static("HIT"),
                );
                return resp;
            }
        }
    }

    // ── FastCGI 后端地址 ─────────────────────────────────────────────────────
    let fcgi_cfg = site.fastcgi.as_ref();

    // Unix socket 路径（优先）或 TCP host:port
    let addr_mode = if let Some(sock) = fcgi_cfg.and_then(|f| f.socket.as_ref()) {
        FcgiAddr::Unix(sock.to_string_lossy().into_owned())
    } else {
        let host = fcgi_cfg.and_then(|f| f.host.as_deref()).unwrap_or("127.0.0.1");
        let port = fcgi_cfg.and_then(|f| f.port).unwrap_or(9000);
        FcgiAddr::Tcp(format!("{}:{}", host, port))
    };

    // ── 连接级信息 ────────────────────────────────────────────────────────
    let is_https  = ctx.req().body().is_tls();
    let peer      = ctx.req().body().socket_addr();
    let peer_ip   = peer.ip().to_string();
    let peer_port = peer.port().to_string();

    // ── 请求行信息 ────────────────────────────────────────────────────────
    let method   = ctx.req().method().as_str();
    let uri      = ctx.req().uri();
    let path_raw = uri.path();
    let query    = uri.query().unwrap_or("");
    let req_uri  = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");

    // ── Host / SERVER_NAME / SERVER_PORT ─────────────────────────────────
    // HTTP/2 用 :authority 伪头，HTTP/1.1 用 Host 头，统一从 uri.authority() 优先读
    let host_hdr: String = uri.authority()
        .map(|a| a.as_str().to_string())
        .or_else(|| ctx.req().headers().get("host")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string()))
        .unwrap_or_default();
    // rsplit_once(':') 分离 host 和 port；不带端口时按协议填默认值
    let (server_name, server_port): (&str, &str) =
        if let Some((h, p)) = host_hdr.rsplit_once(':') {
            (h, p)
        } else if is_https {
            (host_hdr.as_str(), "443")
        } else {
            (host_hdr.as_str(), "80")
        };

    // ── 站点 root ─────────────────────────────────────────────────────────
    let root = match location.root.as_ref().or(site.root.as_ref()) {
        Some(r) => r.to_string_lossy().into_owned(),
        None => return fcgi_error(StatusCode::INTERNAL_SERVER_ERROR, "FastCGI: 未配置 root 目录"),
    };

    // ── SCRIPT_FILENAME / SCRIPT_NAME / PATH_INFO ────────────────────────
    // try_files 已解析出绝对路径时直接用；否则从 URI 路径推断（直接访问 .php）
    let (script_filename, script_name, path_info) = if let Some(abs) = resolved_script {
        let abs_str = abs.to_string_lossy().into_owned();
        let sname = abs_str.strip_prefix(root.trim_end_matches('/'))
            .unwrap_or(&abs_str)
            .replace('\\', "/");
        (abs_str, sname, String::new())
    } else {
        let (sname, pinfo) = split_script_path(path_raw);
        (format!("{}{}", root.trim_end_matches('/'), sname), sname.to_string(), pinfo.to_string())
    };

    // ── 请求体 ────────────────────────────────────────────────────────────
    let max_body = (ctx.state().cfg.global.client_max_body_size as u64) * 1024 * 1024;
    let content_length = ctx.req().headers()
        .get(sweety_web::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    if content_length > max_body && max_body > 0 {
        return fcgi_error(StatusCode::PAYLOAD_TOO_LARGE, "请求体超过 client_max_body_size 限制");
    }
    let content_type = ctx.req().headers()
        .get(sweety_web::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let req_body: Vec<u8> = if matches!(method, "POST" | "PUT" | "PATCH" | "DELETE") {
        use futures_util::StreamExt;
        let mut buf = Vec::with_capacity(content_length.min(16 * 1024 * 1024) as usize);
        let mut body = ctx.body_borrow_mut();
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(b) => buf.extend_from_slice(b.as_ref()),
                Err(_) => break,
            }
        }
        buf
    } else {
        Vec::new()
    };

    // ── REMOTE_ADDR：优先 X-Real-IP，再 X-Forwarded-For，最后直连 IP ─────
    let remote_addr = ctx.req().headers()
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .or_else(|| ctx.req().headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(',').next())
            .map(|s| s.trim().to_string()))
        .unwrap_or_else(|| peer_ip.clone());

    // ── CGI 参数（对标 Nginx /etc/nginx/fastcgi_params）─────────────────
    // 固定参数 + HTTP_* 动态头，预分配避免 realloc
    let mut params: Vec<(String, String)> = Vec::with_capacity(20 + ctx.req().headers().len());
    params.extend([
        // RFC 3875 必须参数
        ("GATEWAY_INTERFACE".into(), "CGI/1.1".into()),
        ("SERVER_SOFTWARE".into(),   "Sweety/0.1".into()),
        ("SERVER_PROTOCOL".into(),   "HTTP/1.1".into()),
        ("SERVER_NAME".into(),       server_name.to_owned()),
        ("SERVER_PORT".into(),       server_port.to_owned()),
        ("REQUEST_METHOD".into(),    method.to_owned()),
        ("REQUEST_URI".into(),       req_uri.to_owned()),
        ("DOCUMENT_URI".into(),      path_raw.to_owned()),
        ("QUERY_STRING".into(),      query.to_owned()),
        ("DOCUMENT_ROOT".into(),     root.clone()),
        ("SCRIPT_FILENAME".into(),   script_filename),
        ("SCRIPT_NAME".into(),       script_name),
        ("PATH_INFO".into(),         path_info.clone()),
        ("PATH_TRANSLATED".into(),   format!("{}{}", root.trim_end_matches('/'), path_info)),
        ("REMOTE_ADDR".into(),       remote_addr.clone()),
        ("REMOTE_HOST".into(),       remote_addr),
        ("REMOTE_PORT".into(),       peer_port),
        // POST 必须参数
        ("CONTENT_TYPE".into(),      content_type.to_owned()),
        ("CONTENT_LENGTH".into(),    itoa::Buffer::new().format(req_body.len()).to_string()),
    ]);

    // HTTPS=on —— 等价 Nginx `fastcgi_param HTTPS $https if_not_empty`
    if is_https {
        params.push(("HTTPS".into(), "on".into()));
    }

    // HTTP_HOST —— HTTP/2 的 :authority 不出现在 headers() 迭代中，必须显式补
    if !host_hdr.is_empty() {
        params.push(("HTTP_HOST".into(), host_hdr.clone()));
    }

    // HTTP/2 Cookie 合并：RFC 7540 §8.1.2.5 规定 HTTP/2 把每个 cookie pair 拆成独立 header 条目
    // PHP-FPM 期望 HTTP_COOKIE 是分号分隔的完整字符串（等价 HTTP/1.1 行为）
    // 必须先收集所有 cookie 头合并，否则 $_COOKIE 只会有最后一个值
    let http_cookie: String = ctx.req().headers().get_all("cookie")
        .iter()
        .map(|v| String::from_utf8_lossy(v.as_bytes()).into_owned())
        .collect::<Vec<_>>()
        .join("; ");
    if !http_cookie.is_empty() {
        params.push(("HTTP_COOKIE".into(), http_cookie));
    }

    // HTTP_* —— 将其余请求头转为 CGI 变量，等价 Nginx `fastcgi_param HTTP_* ...`
    // 跳过已单独处理的头，避免重复
    // Nginx 行为：header value 按字节原样传，不做 UTF-8 验证，非法字节也不丢弃
    // 用 String::from_utf8_lossy 而非 to_str()，保证头不会因非 ASCII 字节被静默丢弃
    for (name, value) in ctx.req().headers().iter() {
        let k = name.as_str();
        if k.eq_ignore_ascii_case("host")
            || k.eq_ignore_ascii_case("cookie")       // 已在上方合并处理
            || k.eq_ignore_ascii_case("content-type")
            || k.eq_ignore_ascii_case("content-length")
        { continue; }
        let mut key = String::with_capacity(5 + k.len());
        key.push_str("HTTP_");
        for c in k.chars() {
            if c == '-' { key.push('_'); } else { key.extend(c.to_uppercase()); }
        }
        let v = String::from_utf8_lossy(value.as_bytes()).into_owned();
        params.push((key, v));
    }

    // ── 发送 FastCGI 请求（连接池 + 超时控制）─────────────────────────────
    let pool      = ctx.state().fcgi_pool.clone();
    let read_tmo  = std::time::Duration::from_secs(pool.read_timeout_secs);
    let (addr_str, is_unix) = match &addr_mode {
        FcgiAddr::Unix(p) => (p.clone(), true),
        FcgiAddr::Tcp(p)  => (p.clone(), false),
    };

    // 缓存键：与删除查询时一致
    let cache_key = fcgi_cache.as_ref().map(|_| {
        crate::middleware::proxy_cache::CacheKey::new(method_str, host_str, req_path)
    });

    // FastCGI I/O 是异步的：等待 PHP-FPM 响应时 tokio 会让出 worker 给其他请求
    // 慢 PHP 请求不会"占死" worker，只是 task 在等待，其他 task 正常调度
    // 真正危险的 CPU 密集操作（gzip 大文件）已通过 spawn_blocking 隔离
    for attempt in 0u8..2 {
        if attempt == 1 {
            pool.evict(&addr_str);
        }
        let conn = match pool.acquire(&addr_str, is_unix).await {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("FastCGI 连接失败 {}: {}", addr_str, e);
                return fcgi_error(StatusCode::BAD_GATEWAY, "PHP-FPM 连接失败");
            }
        };

        let header_result = tokio::time::timeout(
            read_tmo,
            fcgi_send_and_read_headers(conn, &params, &req_body),
        ).await;

        match header_result {
            Ok(Ok(parsed)) => {
                match tokio::time::timeout(
                    read_tmo,
                    build_streaming_response(parsed, pool.clone(), addr_str, fcgi_cache.clone(), cache_key.clone()),
                ).await {
                    Ok(resp) => return resp,
                    Err(_) => return fcgi_error(StatusCode::GATEWAY_TIMEOUT, "PHP-FPM body 读取超时"),
                }
            }
            Ok(Err(e)) if attempt == 0 => {
                tracing::debug!("FastCGI idle 连接失效，重试新建: {}", e);
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

// ─────────────────────────────────────────────
// 流式 FastCGI 响应架构
// ─────────────────────────────────────────────

/// FastCGI 响应头解析结果（发送请求 + 读取响应头后返回）
struct FcgiParsedHeaders {
    /// HTTP 状态码
    status: u16,
    /// 响应头列表
    headers: Vec<(String, String)>,
    /// 已读入但属于 body 的前缀数据
    body_prefix: Vec<u8>,
    /// 连接读端（body 剩余数据从此读取）
    conn: crate::handler::fastcgi_pool::FcgiConn,
    /// body 是否已读完（PHP 输出很短时可能一次就读完了）
    body_done: bool,
}

/// 发送 FastCGI 请求并读取响应头（直到 \r\n\r\n），body 留在 conn 里流式读
/// 用 macro 消除 TCP/Unix 分支重复，不使用 trait object
async fn fcgi_send_and_read_headers(
    conn: crate::handler::fastcgi_pool::FcgiConn,
    params: &[(String, String)],
    stdin_body: &[u8],
) -> anyhow::Result<FcgiParsedHeaders> {
    use crate::handler::fastcgi_pool::FcgiConn;

    // 构建请求包（与连接类型无关）
    let rid: u16 = 1;
    // 预估总包大小：8字节头 + 每个 param 约 (name_len + val_len + 8) 字节 + STDIN
    let params_est: usize = params.iter().map(|(k, v)| k.len() + v.len() + 8).sum();
    let mut pkt = Vec::with_capacity(8 + 8 + params_est + 8 + stdin_body.len() + 64);
    write_fcgi_header(&mut pkt, FCGI_BEGIN_REQUEST, rid, 8, 0);
    pkt.extend_from_slice(&FCGI_RESPONDER.to_be_bytes());
    pkt.push(1); // FCGI_KEEP_CONN
    pkt.extend_from_slice(&[0u8; 5]);
    {
        // 预分配 params 编码缓冲，与 pkt 一起规避连续 realloc
        let mut body = Vec::with_capacity(params_est);
        for (k, v) in params {
            encode_nv_pair(&mut body, k.as_bytes(), v.as_bytes());
        }
        write_fcgi_record(&mut pkt, FCGI_PARAMS, rid, &body);
        write_fcgi_record(&mut pkt, FCGI_PARAMS, rid, &[]);
    }
    write_fcgi_record(&mut pkt, FCGI_STDIN, rid, stdin_body);
    write_fcgi_record(&mut pkt, FCGI_STDIN, rid, &[]);

    // 用宏消除 TCP / Unix 分支重复代码
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
/// 返回 (status, headers, body_prefix, body_done)
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
                if rec.data.is_empty() {
                    body_done = true;
                    break;
                }
                header_buf.extend_from_slice(&rec.data);
                // find_header_end 返回 (body_start, header_text_end)
                if let Some((body_start, hdr_text_end)) = find_header_end(&header_buf) {
                    body_prefix = header_buf[body_start..].to_vec();
                    header_buf.truncate(hdr_text_end); // 只保留纯头部文本（不含 \r\n\r\n）
                    break;
                }
            }
            t if t == FCGI_STDERR => {
                if let Ok(s) = std::str::from_utf8(&rec.data) {
                    if !s.trim().is_empty() {
                        tracing::warn!("PHP-FPM stderr: {}", s.trim());
                    }
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
    // with_capacity 预分配，避免 read_exact 循环中 realloc
    let mut buf = vec![0u8; total];
    if total > 0 { stream.read_exact(&mut buf).await?; }
    buf.truncate(content_len);
    Ok(FcgiRecord { rec_type, data: buf })
}

/// FastCGI 响应处理：流式转发，首字节延迟最低
/// - body 已读完或 body < 4MB：全量收集再返回，并写入缓存
/// - body >= 4MB：流式转发（过大不缓存）
async fn build_streaming_response(
    parsed: FcgiParsedHeaders,
    pool: std::sync::Arc<crate::handler::fastcgi_pool::FcgiPool>,
    addr: String,
    fcgi_cache: Option<std::sync::Arc<crate::middleware::proxy_cache::ProxyCache>>,
    cache_key: Option<crate::middleware::proxy_cache::CacheKey>,
) -> WebResponse {
    // body 已全部读完，写缓存并直接返回
    if parsed.body_done {
        pool.release(&addr, parsed.conn);
        write_fcgi_cache(&fcgi_cache, &cache_key, parsed.status, &parsed.headers, &parsed.body_prefix);
        return make_complete_response(parsed.status, parsed.headers, parsed.body_prefix);
    }

    // 全量收集 body（< 4MB），写缓存再返回
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
                if body.len() > 4 * 1024 * 1024 {
                    use_stream = true;
                    break;
                }
            }
            t if t == FCGI_STDERR => {
                if let Ok(s) = std::str::from_utf8(&rec.data) {
                    if !s.trim().is_empty() { tracing::warn!("PHP-FPM stderr: {}", s.trim()); }
                }
            }
            _ => break, // FCGI_END_REQUEST
        }
    }

    if use_stream {
        // body 过大，转流式路径（不缓存）
        return stream_remaining(body, conn, pool, addr, parsed.status, parsed.headers).await;
    }

    pool.release(&addr, conn);
    write_fcgi_cache(&fcgi_cache, &cache_key, parsed.status, &parsed.headers, &body);
    make_complete_response(parsed.status, parsed.headers, body)
}

/// 尝试将 FastCGI 响应写入缓存
fn write_fcgi_cache(
    cache: &Option<std::sync::Arc<crate::middleware::proxy_cache::ProxyCache>>,
    key: &Option<crate::middleware::proxy_cache::CacheKey>,
    status: u16,
    headers: &[(String, String)],
    body: &[u8],
) {
    if let (Some(cache), Some(key)) = (cache, key) {
        if cache.is_cacheable(status, headers) {
            cache.set(
                key.clone(),
                status,
                headers.to_vec(),
                bytes::Bytes::copy_from_slice(body),
            );
        }
    }
}

/// 流式转发剩余 FCGI STDOUT——已有部分 body 缓冲，用 spawn task 继续读取
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
                _ => { pool.release(&addr, conn); return; } // FCGI_END_REQUEST
            }
        }
    });

    let http_status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    // 304/204/205/1xx 不能带 body
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

/// 构造完整响应（body 已全量，无需 stream）
fn make_complete_response(status: u16, headers: Vec<(String, String)>, body: Vec<u8>) -> WebResponse {
    use sweety_web::http::header::HeaderName;
    let http_status = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    // 304/204/205/1xx 不能带 body（HTTP 规范 + H2 dispatcher 要求）
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

// ─────────────────────────────────────────────
// FCGI 记录读取 + 头部解析辅助
// ─────────────────────────────────────────────

struct FcgiRecord {
    rec_type: u8,
    data: Vec<u8>,
}

/// 从 FcgiConn 读一条 FCGI 记录—— enum dispatch，无 trait object 开销
async fn read_fcgi_conn(conn: &mut crate::handler::fastcgi_pool::FcgiConn) -> std::io::Result<FcgiRecord> {
    use tokio::io::AsyncReadExt;
    use crate::handler::fastcgi_pool::FcgiConn;
    let mut hdr = [0u8; 8];
    match conn {
        FcgiConn::Tcp(s)  => s.read_exact(&mut hdr).await?,
        #[cfg(unix)]
        FcgiConn::Unix(s) => s.read_exact(&mut hdr).await?,
    };
    let rec_type    = hdr[1];
    let content_len = u16::from_be_bytes([hdr[4], hdr[5]]) as usize;
    let padding_len = hdr[6] as usize;
    let total = content_len + padding_len;
    let mut buf = vec![0u8; total];
    if total > 0 {
        match conn {
            FcgiConn::Tcp(s)  => s.read_exact(&mut buf).await?,
            #[cfg(unix)]
            FcgiConn::Unix(s) => s.read_exact(&mut buf).await?,
        };
    }
    buf.truncate(content_len);
    Ok(FcgiRecord { rec_type, data: buf })
}

/// 寻找 HTTP 头尾分隔符（\r\n\r\n 或 \n\n），返回 body 起始位置和头部文本长度
/// 返回 (body_start, header_text_len)
fn find_header_end(buf: &[u8]) -> Option<(usize, usize)> {
    for i in 0..buf.len().saturating_sub(3) {
        if buf[i] == b'\r' && buf[i+1] == b'\n' && buf[i+2] == b'\r' && buf[i+3] == b'\n' {
            return Some((i + 4, i)); // body 从 i+4 开始，头部文本到 i
        }
    }
    for i in 0..buf.len().saturating_sub(1) {
        if buf[i] == b'\n' && buf[i+1] == b'\n' {
            return Some((i + 2, i)); // body 从 i+2 开始，头部文本到 i
        }
    }
    None
}

/// 找头部文本结束位置（不包含分隔符），为兼容旧调用保留
#[allow(dead_code)]
fn find_header_text_end(buf: &[u8]) -> Option<usize> {
    find_header_end(buf).map(|(_, text_end)| text_end)
}

/// 解析 FastCGI 响应头文本（不含 body），返回状态码和头列表
fn parse_fcgi_headers(header_str: &str) -> (u16, Vec<(String, String)>) {
    let mut status_code: u16 = 200;
    let mut response_headers: Vec<(String, String)> = Vec::new();
    for line in header_str.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        if let Some(rest) = line.strip_prefix("Status:").or_else(|| line.strip_prefix("status:")) {
            if let Some(code_str) = rest.trim().split_whitespace().next() {
                status_code = code_str.parse().unwrap_or(200);
            }
            continue;
        }
        if let Some(colon) = line.find(':') {
            let name  = line[..colon].trim().to_string();
            let value = line[colon+1..].trim().to_string();
            response_headers.push((name, value));
        }
    }
    (status_code, response_headers)
}

/// 全量缓冲解析（已被流式架构替代，保留备用）
#[allow(dead_code)]
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
    use sweety_web::http::header::HeaderName;
    for (k, v) in &response_headers {
        if let (Ok(name), Ok(val)) = (
            HeaderName::from_bytes(k.as_bytes()),
            HeaderValue::from_bytes(v.as_bytes()),
        ) {
            resp.headers_mut().append(name, val);
        }
    }

    // 若 PHP 没有输出 Content-Type，默认 text/html
    if !response_headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
        resp.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    }

    // 设置 Content-Length（PHP 已知输出长度时有利于 keep-alive 复用）
    if let Ok(v) = HeaderValue::from_bytes(body_len.to_string().as_bytes()) {
        resp.headers_mut().insert(
            sweety_web::http::header::CONTENT_LENGTH,
            v,
        );
    }

    resp
}
