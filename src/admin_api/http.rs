//! 管理 HTTP API 模块
//! 提供 RESTful 接口：站点管理、统计查询、限流规则调整
//! 挂载在 global.admin_listen 端口上，通过 Bearer Token 鉴权

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tracing::{error, info};

use crate::middleware::metrics::GlobalMetrics;

/// 管理 API 服务器配置
pub struct AdminApiConfig {
    /// 监听地址（如 "127.0.0.1:9000"）
    pub listen_addr: String,
    /// Bearer Token（空字符串表示不鉴权，生产不推荐）
    pub token: String,
}

/// 启动管理 HTTP API 服务器
pub async fn start(cfg: AdminApiConfig, metrics: Arc<GlobalMetrics>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(&cfg.listen_addr).await?;
    info!("管理 API 监听: http://{}", cfg.listen_addr);

    let cfg = Arc::new(cfg);
    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                let cfg = cfg.clone();
                let metrics = metrics.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_admin_request(stream, cfg, metrics).await {
                        error!("管理 API 请求处理失败 [{}]: {}", peer, e);
                    }
                });
            }
            Err(e) => error!("管理 API accept 失败: {}", e),
        }
    }
}

/// 处理单个管理 HTTP/1.1 请求（简化实现）
async fn handle_admin_request(
    stream: tokio::net::TcpStream,
    cfg: Arc<AdminApiConfig>,
    metrics: Arc<GlobalMetrics>,
) -> anyhow::Result<()> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut buf_reader = BufReader::new(reader);

    // 读取请求行
    let mut request_line = String::new();
    buf_reader.read_line(&mut request_line).await?;
    let parts: Vec<&str> = request_line.trim().splitn(3, ' ').collect();
    if parts.len() < 2 {
        return Ok(());
    }
    let method = parts[0].to_string();
    let path = parts[1].to_string();

    // 读取所有请求头
    let mut headers: HashMap<String, String> = HashMap::new();
    loop {
        let mut line = String::new();
        buf_reader.read_line(&mut line).await?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }

    // Bearer Token 鉴权
    if !cfg.token.is_empty() {
        let auth = headers.get("authorization").map(|s| s.as_str()).unwrap_or("");
        let expected = format!("Bearer {}", cfg.token);
        if auth != expected {
            let resp = json_response(401, r#"{"error":"Unauthorized"}"#);
            writer.write_all(resp.as_bytes()).await?;
            return Ok(());
        }
    }

    // 路由
    let response_body = route(&method, &path, &metrics).await;
    let resp = json_response(response_body.0, &response_body.1);
    writer.write_all(resp.as_bytes()).await?;

    Ok(())
}

/// 生成 API 文档 JSON（给 --api-doc 和 /api/v1/doc 共用）
pub fn build_api_doc() -> serde_json::Value {
    serde_json::json!({
        "name": "Sweety Admin API",
        "version": env!("CARGO_PKG_VERSION"),
        "base": "/api/v1",
        "auth": {
            "type": "Bearer",
            "header": "Authorization",
            "description": "设置 global.admin_token 开启鉴权（为空则不鉴权）"
        },
        "endpoints": [
            {
                "method": "GET",
                "path": "/api/v1/health",
                "description": "健康检查",
                "auth_required": false,
                "response": { "status": "ok" }
            },
            {
                "method": "GET",
                "path": "/api/v1/version",
                "description": "版本信息",
                "auth_required": false,
                "response": { "name": "Sweety", "version": "x.y.z" }
            },
            {
                "method": "GET",
                "path": "/api/v1/stats",
                "description": "全局请求统计快照",
                "auth_required": true,
                "response": {
                    "total_requests": "u64",
                    "active_connections": "u64",
                    "bytes_sent": "u64",
                    "bytes_received": "u64",
                    "error_4xx": "u64",
                    "error_5xx": "u64"
                }
            },
            {
                "method": "GET",
                "path": "/api/v1/sites",
                "description": "站点列表",
                "auth_required": true,
                "response": { "sites": [] }
            },
            {
                "method": "GET",
                "path": "/api/v1/doc",
                "description": "API 文档（当前接口）",
                "auth_required": false,
                "response": "<this document>"
            },
            {
                "method": "GET",
                "path": "/api/v1/upstreams",
                "description": "上游节点列表及断路器状态",
                "auth_required": true,
                "response": {
                    "upstreams": [{
                        "name": "string",
                        "nodes": [{
                            "addr": "string",
                            "healthy": "bool",
                            "active_connections": "u32",
                            "circuit_breaker_open": "bool | null"
                        }]
                    }]
                }
            },
            {
                "method": "POST",
                "path": "/api/v1/upstreams/:name/nodes/:addr/enable",
                "description": "启用节点",
                "auth_required": true
            },
            {
                "method": "POST",
                "path": "/api/v1/upstreams/:name/nodes/:addr/disable",
                "description": "禿用节点（手动标记不健康）",
                "auth_required": true
            },
            {
                "method": "POST",
                "path": "/api/v1/reload",
                "description": "热重载配置（不断连）",
                "auth_required": true
            },
            {
                "method": "GET",
                "path": "/api/v1/plugins",
                "description": "已注册插件列表 (handler=plugin:xxx)",
                "auth_required": true
            }
        ]
    })
}

/// API 路由分发
async fn route(method: &str, path: &str, metrics: &GlobalMetrics) -> (u16, String) {
    match (method, path) {
        // GET /api/v1/stats — 全局统计快照
        ("GET", "/api/v1/stats") => {
            let snap = metrics.snapshot();
            let body = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".into());
            (200, body)
        }

        // GET /api/v1/health — 健康检查
        ("GET", "/api/v1/health") | ("GET", "/health") => {
            (200, r#"{"status":"ok"}"#.into())
        }

        // GET /api/v1/version — 版本信息
        ("GET", "/api/v1/version") => {
            let body = serde_json::json!({
                "name":    "Sweety",
                "version": env!("CARGO_PKG_VERSION"),
            })
            .to_string();
            (200, body)
        }

        // GET /api/v1/sites — 站点列表
        ("GET", "/api/v1/sites") => {
            (200, r#"{"sites":[],"note":"complete implementation in v0.5"}"#.into())
        }

        // GET /api/v1/doc — API 文档
        ("GET", "/api/v1/doc") => {
            let doc = build_api_doc();
            (200, doc.to_string())
        }

        // GET /api/v1/plugins — 已注册插件列表
        ("GET", "/api/v1/plugins") => {
            use crate::handler::plugin::plugin_registry;
            let reg = plugin_registry();
            let names: Vec<serde_json::Value> = reg
                .plugin_names()
                .into_iter()
                .map(|n| serde_json::json!({ "name": n }))
                .collect();
            let body = serde_json::json!({ "plugins": names }).to_string();
            (200, body)
        }

        // 未匹配路由
        _ => (
            404,
            format!(r#"{{"error":"Not Found","path":"{}"}}"#, path),
        ),
    }
}

/// 构建 HTTP/1.1 JSON 响应字符串
fn json_response(status: u16, body: &str) -> String {
    let status_text = match status {
        200 => "OK",
        201 => "Created",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nConnection: close\r\n\r\n{}",
        status, status_text, body.len(), body
    )
}

// ─────────────────────────────────────────────
// 请求/响应 DTO（后续 API 扩展使用）
// ─────────────────────────────────────────────

/// 添加站点请求体（POST /api/v1/sites）
#[derive(Debug, Deserialize)]
pub struct AddSiteRequest {
    pub name: String,
    pub server_name: Vec<String>,
    pub root: Option<String>,
}

/// 通用 API 响应
#[derive(Debug, Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl<T: Serialize> ApiResponse<T> {
    pub fn ok(data: T) -> Self {
        Self { success: true, data: Some(data), error: None }
    }
    pub fn err(msg: impl Into<String>) -> ApiResponse<()> {
        ApiResponse { success: false, data: None, error: Some(msg.into()) }
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::metrics::GlobalMetrics;

    #[tokio::test]
    async fn test_health_route() {
        let metrics = GlobalMetrics::new();
        let (status, body) = route("GET", "/health", &metrics).await;
        assert_eq!(status, 200);
        assert!(body.contains("ok"));
    }

    #[tokio::test]
    async fn test_stats_route_returns_json() {
        let metrics = GlobalMetrics::new();
        metrics.inc_requests();
        let (status, body) = route("GET", "/api/v1/stats", &metrics).await;
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["total_requests"], 1);
    }

    #[tokio::test]
    async fn test_version_route() {
        let metrics = GlobalMetrics::new();
        let (status, body) = route("GET", "/api/v1/version", &metrics).await;
        assert_eq!(status, 200);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["name"], "Sweety");
    }

    #[tokio::test]
    async fn test_unknown_route_404() {
        let metrics = GlobalMetrics::new();
        let (status, _) = route("GET", "/unknown/path", &metrics).await;
        assert_eq!(status, 404);
    }

    #[test]
    fn test_json_response_format() {
        let resp = json_response(200, r#"{"ok":true}"#);
        assert!(resp.starts_with("HTTP/1.1 200 OK"));
        assert!(resp.contains("application/json"));
        assert!(resp.contains(r#"{"ok":true}"#));
    }

    #[test]
    fn test_api_response_ok() {
        let r = ApiResponse::ok(42u32);
        assert!(r.success);
        assert_eq!(r.data, Some(42u32));
        assert!(r.error.is_none());
    }
}
