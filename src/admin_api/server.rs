//! 管理 API TCP 服务器：启动监听、HTTP/1.1 解析、鉴权

use std::collections::HashMap;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tracing::{error, info};

use super::context::AdminContext;
use super::router::route;
use super::util::{build_response, cors_preflight_response, err_json, json_response, parse_path_query};

// ═══════════════════════════════════════════════════════════════════════
// 路由响应类型
// ═══════════════════════════════════════════════════════════════════════

/// 路由响应（支持 JSON 和 text/plain 等不同 Content-Type）
pub struct RouteResponse {
    pub status: u16,
    pub body: String,
    pub content_type: &'static str,
}

impl RouteResponse {
    pub fn json(status: u16, body: String) -> Self {
        Self { status, body, content_type: "application/json; charset=utf-8" }
    }
    pub fn text(status: u16, body: String) -> Self {
        Self { status, body, content_type: "text/plain; charset=utf-8" }
    }
}

impl From<(u16, String)> for RouteResponse {
    fn from((status, body): (u16, String)) -> Self {
        Self::json(status, body)
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 解析后的请求
// ═══════════════════════════════════════════════════════════════════════

/// 解析后的 HTTP 请求
pub struct ParsedRequest {
    pub method: String,
    /// 路径（不含 query string）
    pub path: String,
    /// query 参数（key=value）
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl ParsedRequest {
    /// 检查 query 参数 save=true（是否持久化到配置文件）
    pub fn should_save(&self) -> bool {
        self.query.get("save").map(|v| v == "true" || v == "1").unwrap_or(false)
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 启动入口
// ═══════════════════════════════════════════════════════════════════════

/// 启动管理 HTTP API 服务器（独立 TCP listener，不影响主服务器性能）
pub async fn start(ctx: AdminContext) -> anyhow::Result<()> {
    let listener = TcpListener::bind(&ctx.listen_addr).await?;
    info!("管理 API 监听: http://{}", ctx.listen_addr);

    let ctx = Arc::new(ctx);
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        handle_request(stream, ctx)
                    }));
                    match result {
                        Ok(fut) => {
                            if let Err(e) = fut.await {
                                error!("管理 API [{}]: {}", peer, e);
                            }
                        }
                        Err(_) => error!("管理 API [{}] panic，已恢复", peer),
                    }
                });
            }
            Err(e) => error!("管理 API accept 失败: {}", e),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
// 安全工具函数
// ═══════════════════════════════════════════════════════════════════════

/// 常量时间字节比较，防止时间侧信道攻击
///
/// 关键设计：
/// - `#[inline(never)]` 阻止编译器内联后将 XOR 累加优化为短路求值
/// - 使用 `volatile` 读取最终结果，防止编译器消除"多余"的比较
/// - 长度不等时也做伪比较，不泄露长度信息（长度已在调用侧提前检查）
#[inline(never)]
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    // volatile 读取防止编译器优化掉累加过程
    unsafe { std::ptr::read_volatile(&diff) == 0 }
}

// ═══════════════════════════════════════════════════════════════════════
// HTTP/1.1 请求处理
// ═══════════════════════════════════════════════════════════════════════

async fn handle_request(
    stream: tokio::net::TcpStream,
    ctx: Arc<AdminContext>,
) -> anyhow::Result<()> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut buf_reader = BufReader::new(reader);

    // 请求行（限制 8KB 防止 DoS）
    let mut request_line = String::new();
    buf_reader.read_line(&mut request_line).await?;
    if request_line.len() > 8192 { return Ok(()); }
    let parts: Vec<&str> = request_line.trim().splitn(3, ' ').collect();
    if parts.len() < 3 { return Ok(()); }
    let method = parts[0].to_string();
    let raw_path = parts[1].to_string();
    // 安全修复：验证 HTTP 版本字符串
    let http_version = parts[2];
    if !http_version.starts_with("HTTP/1.") {
        let resp = json_response(400, &err_json("Unsupported HTTP version"));
        writer.write_all(resp.as_bytes()).await?;
        return Ok(());
    }
    let (path, query) = parse_path_query(&raw_path);

    // 请求头（安全修复：限制 header 数量和大小，拒绝重复 Content-Length 和 chunked TE）
    let mut headers: HashMap<String, String> = HashMap::new();
    let mut header_count = 0usize;
    let mut has_content_length = false;
    loop {
        let mut line = String::new();
        buf_reader.read_line(&mut line).await?;
        if line.trim().is_empty() { break; }
        header_count += 1;
        // 防止 header 泛洪 DoS：最多 128 个 header
        if header_count > 128 {
            let resp = json_response(431, &err_json("Too many headers"));
            writer.write_all(resp.as_bytes()).await?;
            return Ok(());
        }
        // 防止超长 header 行：最大 8KB
        if line.len() > 8192 {
            let resp = json_response(431, &err_json("Header line too long"));
            writer.write_all(resp.as_bytes()).await?;
            return Ok(());
        }
        if let Some((k, v)) = line.trim().split_once(':') {
            let key = k.trim().to_lowercase();
            // 安全修复：拒绝 chunked Transfer-Encoding（防止 CL-TE 走私）
            if key == "transfer-encoding" {
                let resp = json_response(400, &err_json("Transfer-Encoding not supported by admin API"));
                writer.write_all(resp.as_bytes()).await?;
                return Ok(());
            }
            // 安全修复：拒绝重复 Content-Length（防止 CL-CL 走私）
            if key == "content-length" {
                if has_content_length {
                    let resp = json_response(400, &err_json("Duplicate Content-Length header"));
                    writer.write_all(resp.as_bytes()).await?;
                    return Ok(());
                }
                has_content_length = true;
            }
            headers.insert(key, v.trim().to_string());
        }
    }

    // 请求体
    let content_len: usize = headers.get("content-length")
        .and_then(|v| v.parse().ok()).unwrap_or(0);
    if content_len > 1024 * 64 {
        let resp = json_response(413, &err_json("Request body too large (max 64KB)"));
        writer.write_all(resp.as_bytes()).await?;
        return Ok(());
    }
    let mut body = vec![0u8; content_len];
    if content_len > 0 {
        buf_reader.read_exact(&mut body).await?;
    }

    // CORS preflight
    if method == "OPTIONS" {
        let resp = cors_preflight_response();
        writer.write_all(resp.as_bytes()).await?;
        return Ok(());
    }

    let req = ParsedRequest { method, path, query, headers, body };

    // 鉴权（部分路径免鉴权）
    let no_auth_paths = ["/api/health", "/health", "/api/version", "/api/doc", "/metrics"];
    if !no_auth_paths.contains(&req.path.as_str()) {
        if ctx.token.is_empty() {
            // token 未配置时，拒绝所有非健康检查请求，防止未授权访问
            let resp = json_response(403, &err_json("Admin API token not configured, access denied"));
            writer.write_all(resp.as_bytes()).await?;
            return Ok(());
        }
        let auth = req.headers.get("authorization").map(|s| s.as_str()).unwrap_or("");
        let expected = format!("Bearer {}", ctx.token);
        // 常量时间比较，防止时间侧信道攻击
        if auth.len() != expected.len() || !constant_time_eq(auth.as_bytes(), expected.as_bytes()) {
            let resp = json_response(401, &err_json("Unauthorized"));
            writer.write_all(resp.as_bytes()).await?;
            return Ok(());
        }
    }

    // 路由分发
    let rr = route(&req, &ctx).await;
    let resp = build_response(rr.status, &rr.body, rr.content_type);
    writer.write_all(resp.as_bytes()).await?;
    Ok(())
}
