//! Upstreams 端点：列表 / 详情 / 节点操作 / Caddy 兼容格式

use std::sync::Arc;
use std::sync::atomic::Ordering;

use tracing::info;

use crate::admin_api::context::AdminContext;
use crate::admin_api::server::ParsedRequest;
use crate::admin_api::util::{ok_json, err_json, urldecode};

pub fn route_upstreams_list(ctx: &AdminContext) -> (u16, String) {
    let sites = ctx.registry.all_sites();
    let mut upstreams: Vec<serde_json::Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for site in &sites {
        for (name, pool) in &site.upstream_pools {
            if !seen.insert(name.clone()) { continue; }
            let nodes: Vec<serde_json::Value> = pool.nodes.iter().map(|n| {
                serde_json::json!({
                    "addr": n.addr.as_str(),
                    "weight": n.weight,
                    "healthy": n.is_healthy(),
                    "available": n.is_available(),
                    "active_connections": n.active_connections.load(Ordering::Relaxed),
                    "fail_count": n.fail_count.load(Ordering::Relaxed),
                    "tls": n.tls,
                    "http2": n.http2,
                    "circuit_breaker_open": n.circuit_breaker.as_ref().map(|cb| cb.is_open()),
                })
            }).collect();
            upstreams.push(serde_json::json!({
                "name": name,
                "site": site.name,
                "strategy": format!("{:?}", site.upstreams.iter()
                    .find(|u| &u.name == name)
                    .map(|u| &u.strategy)),
                "node_count": pool.nodes.len(),
                "healthy_count": pool.nodes.iter().filter(|n| n.is_healthy()).count(),
                "nodes": nodes,
            }));
        }
    }
    let body = serde_json::json!({ "count": upstreams.len(), "upstreams": upstreams });
    (200, body.to_string())
}

pub fn route_upstream_detail(ctx: &AdminContext, name: &str) -> (u16, String) {
    let sites = ctx.registry.all_sites();
    for site in &sites {
        if let Some(pool) = site.upstream_pools.get(name) {
            let nodes: Vec<serde_json::Value> = pool.nodes.iter().map(|n| {
                serde_json::json!({
                    "addr": n.addr.as_str(),
                    "weight": n.weight,
                    "healthy": n.is_healthy(),
                    "available": n.is_available(),
                    "active_connections": n.active_connections.load(Ordering::Relaxed),
                    "fail_count": n.fail_count.load(Ordering::Relaxed),
                    "tls": n.tls,
                    "tls_sni": n.tls_sni,
                    "tls_insecure": n.tls_insecure,
                    "http2": n.http2,
                    "send_proxy_protocol": n.send_proxy_protocol,
                    "upstream_host": n.upstream_host,
                    "circuit_breaker_open": n.circuit_breaker.as_ref().map(|cb| cb.is_open()),
                })
            }).collect();
            let body = serde_json::json!({
                "name": name,
                "site": site.name,
                "nodes": nodes,
                "keepalive_requests": pool.keepalive_requests,
                "keepalive_time": pool.keepalive_time,
                "keepalive_max_idle": pool.keepalive_max_idle,
                "connect_timeout": pool.connect_timeout,
                "read_timeout": pool.read_timeout,
                "write_timeout": pool.write_timeout,
                "retry": pool.retry,
                "retry_timeout": pool.retry_timeout,
            });
            return (200, body.to_string());
        }
    }
    (404, err_json(&format!("上游组 '{}' 不存在", name)))
}

/// 解析 /api/upstreams/:name/nodes/:addr/(enable|disable|weight) 路径
pub async fn route_upstream_node_action(ctx: &AdminContext, req: &ParsedRequest) -> Option<(u16, String)> {
    let rest = req.path.strip_prefix("/api/upstreams/")?;
    let (name, rest) = rest.split_once("/nodes/")?;
    let (addr_encoded, action) = rest.rsplit_once('/')?;
    let addr = urldecode(addr_encoded);

    let sites = ctx.registry.all_sites();
    for site in &sites {
        if let Some(pool) = site.upstream_pools.get(name) {
            if let Some(node) = pool.nodes.iter().find(|n| n.addr.as_str() == addr) {
                match action {
                    "enable" if req.method == "POST" => {
                        node.mark_healthy();
                        info!("管理 API 启用节点: {} / {}", name, addr);
                        return Some((200, ok_json(&format!("节点 {} 已启用", addr))));
                    }
                    "disable" if req.method == "POST" => {
                        node.mark_unhealthy();
                        info!("管理 API 禁用节点: {} / {}", name, addr);
                        return Some((200, ok_json(&format!("节点 {} 已禁用", addr))));
                    }
                    "weight" if req.method == "PUT" => {
                        match serde_json::from_slice::<serde_json::Value>(&req.body) {
                            Ok(v) => {
                                if let Some(w) = v.get("weight").and_then(|w| w.as_u64()) {
                                    let node_ptr = Arc::as_ptr(node) as *mut crate::handler::reverse_proxy::lb::NodeState;
                                    // SAFETY: weight 字段在 pick() 中只做读取，admin API 单线程写入
                                    unsafe { (*node_ptr).weight = w as u32; }
                                    info!("管理 API 修改节点权重: {} / {} → {}", name, addr, w);
                                    return Some((200, ok_json(&format!("节点 {} 权重已更新为 {}", addr, w))));
                                }
                                return Some((400, err_json("缺少 weight 字段")));
                            }
                            Err(e) => return Some((400, err_json(&format!("JSON 解析失败: {}", e)))),
                        }
                    }
                    _ => {}
                }
            }
            return Some((404, err_json(&format!("节点 '{}' 在上游组 '{}' 中不存在", addr, name))));
        }
    }
    Some((404, err_json(&format!("上游组 '{}' 不存在", name))))
}

/// GET /reverse_proxy/upstreams — Caddy 兼容格式
pub fn route_caddy_upstreams(ctx: &AdminContext) -> (u16, String) {
    let sites = ctx.registry.all_sites();
    let mut result: Vec<serde_json::Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for site in &sites {
        for (_name, pool) in &site.upstream_pools {
            for node in &pool.nodes {
                let addr = node.addr.as_str().to_string();
                if !seen.insert(addr.clone()) { continue; }
                result.push(serde_json::json!({
                    "address": addr,
                    "healthy": node.is_healthy(),
                    "num_requests": node.active_connections.load(Ordering::Relaxed),
                    "fails": node.fail_count.load(Ordering::Relaxed),
                }));
            }
        }
    }
    (200, serde_json::to_string(&result).unwrap_or_else(|_| "[]".into()))
}
