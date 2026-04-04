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
// HTTP/1.1 请求处理
// ═══════════════════════════════════════════════════════════════════════

async fn handle_request(
    stream: tokio::net::TcpStream,
    ctx: Arc<AdminContext>,
) -> anyhow::Result<()> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut buf_reader = BufReader::new(reader);

    // 请求行
    let mut request_line = String::new();
    buf_reader.read_line(&mut request_line).await?;
    let parts: Vec<&str> = request_line.trim().splitn(3, ' ').collect();
    if parts.len() < 2 { return Ok(()); }
    let method = parts[0].to_string();
    let raw_path = parts[1].to_string();
    let (path, query) = parse_path_query(&raw_path);

    // 请求头
    let mut headers: HashMap<String, String> = HashMap::new();
    loop {
        let mut line = String::new();
        buf_reader.read_line(&mut line).await?;
        if line.trim().is_empty() { break; }
        if let Some((k, v)) = line.trim().split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }

    // 请求体
    let content_len: usize = headers.get("content-length")
        .and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0u8; content_len.min(1024 * 64)]; // 最大 64KB
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
    if !ctx.token.is_empty() && !no_auth_paths.contains(&req.path.as_str()) {
        let auth = req.headers.get("authorization").map(|s| s.as_str()).unwrap_or("");
        let expected = format!("Bearer {}", ctx.token);
        if auth != expected {
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
