//! 运行时端点：certs / cache / connections / plugins / logs / metrics

use std::sync::atomic::Ordering;

use tracing::info;

use crate::admin_api::context::AdminContext;
use crate::admin_api::server::{ParsedRequest, RouteResponse};
use crate::admin_api::util::{ok_json, err_json, read_cert_expiry};

// ── Certificates ────────────────────────────────────────

pub fn route_certs_list(ctx: &AdminContext) -> (u16, String) {
    let cfg = ctx.cfg.load();
    let mut certs: Vec<serde_json::Value> = Vec::new();
    for site in &cfg.sites {
        if let Some(ref tls) = site.tls {
            let cert_path = tls.cert.as_ref().map(|p| p.display().to_string());
            let key_path = tls.key.as_ref().map(|p| p.display().to_string());
            let expiry = cert_path.as_ref().and_then(|p| read_cert_expiry(p));
            certs.push(serde_json::json!({
                "site": site.name,
                "domains": site.server_name,
                "cert_path": cert_path,
                "key_path": key_path,
                "acme": tls.acme,
                "expiry": expiry,
                "protocols": tls.protocols,
            }));
        }
    }
    let body = serde_json::json!({ "count": certs.len(), "certificates": certs });
    (200, body.to_string())
}

pub fn route_certs_reload(ctx: &AdminContext) -> (u16, String) {
    let (status, body) = super::config::route_config_reload(ctx);
    if status == 200 {
        (200, ok_json("证书已重新加载"))
    } else {
        (status, body)
    }
}

/// POST /api/certs/acme/renew[?site=name] — 立即触发 ACME 证书续期
///
/// - 不指定 site 参数：续期所有 ACME 站点
/// - 指定 site=my-site：仅续期该站点
/// - 申请失败继续使用当前证书，不影响服务
/// - ACME 申请可能耗时数分钟，在后台异步执行，API 立即返回
pub fn route_certs_acme_renew(ctx: &AdminContext, req: &crate::admin_api::server::ParsedRequest) -> (u16, String) {
    let site_filter = req.query.get("site").cloned();
    let cfg = ctx.cfg.load();
    let resolvers = ctx.sni_resolvers.clone();

    // 检查是否有 ACME 站点
    let acme_sites: Vec<&str> = cfg.sites.iter()
        .filter(|s| s.tls.as_ref().map(|t| t.acme).unwrap_or(false))
        .filter(|s| site_filter.as_ref().map(|f| s.name == *f).unwrap_or(true))
        .map(|s| s.name.as_str())
        .collect();

    if acme_sites.is_empty() {
        let msg = match &site_filter {
            Some(name) => format!("站点 '{}' 不存在或未启用 ACME", name),
            None => "没有启用 ACME 的站点".to_string(),
        };
        return (404, err_json(&msg));
    }

    let site_names: Vec<String> = acme_sites.iter().map(|s| s.to_string()).collect();
    let cfg_clone = cfg.clone();
    let filter_clone = site_filter.clone();

    // 在后台异步执行 ACME 续期，不阻塞 API 响应
    tokio::spawn(async move {
        let filter_ref = filter_clone.as_deref();
        let (triggered, _skipped, errors) = crate::server::acme::acme_renew_now(
            &cfg_clone, &resolvers, filter_ref,
        ).await;
        if errors.is_empty() {
            info!("ACME 即时续期完成: {} 个域名成功", triggered);
        } else {
            info!("ACME 即时续期完成: {} 个成功, {} 个失败", triggered, errors.len());
        }
    });

    let body = serde_json::json!({
        "message": "ACME 续期已在后台触发",
        "sites": site_names,
        "note": "申请可能耗时数分钟，请查看日志获取结果。失败时继续使用当前证书。"
    });
    (202, body.to_string())
}

// ── Cache ───────────────────────────────────────────────

pub fn route_cache_stats(ctx: &AdminContext) -> (u16, String) {
    let sites = ctx.registry.all_sites();
    let mut caches: Vec<serde_json::Value> = Vec::new();
    for site in &sites {
        if let Some(ref cache) = site.proxy_cache_arc {
            caches.push(serde_json::json!({
                "site": site.name,
                "type": "proxy_cache",
                "stats": cache.stats(),
            }));
        }
        if let Some(ref cache) = site.fcgi_cache_arc {
            caches.push(serde_json::json!({
                "site": site.name,
                "type": "fcgi_cache",
                "stats": cache.stats(),
            }));
        }
    }
    let body = serde_json::json!({ "caches": caches });
    (200, body.to_string())
}

pub fn route_cache_purge(ctx: &AdminContext) -> (u16, String) {
    let sites = ctx.registry.all_sites();
    let mut purged = 0usize;
    for site in &sites {
        if let Some(ref cache) = site.proxy_cache_arc {
            cache.purge_all();
            purged += 1;
        }
        if let Some(ref cache) = site.fcgi_cache_arc {
            cache.purge_all();
            purged += 1;
        }
    }
    info!("管理 API 清除缓存: {} 个缓存实例", purged);
    (200, ok_json(&format!("已清除 {} 个缓存实例", purged)))
}

// ── Connections ─────────────────────────────────────────

pub fn route_connections(ctx: &AdminContext) -> (u16, String) {
    let snap = ctx.metrics.snapshot();
    let cfg = ctx.cfg.load();
    let body = serde_json::json!({
        "active_connections": ctx.active_connections.load(Ordering::Relaxed),
        "max_connections": cfg.global.max_connections,
        "active_requests": snap.active_requests,
        "active_ws_connections": snap.active_ws_connections,
    });
    (200, body.to_string())
}

// ── Plugins ─────────────────────────────────────────────

pub fn route_plugins() -> (u16, String) {
    use crate::handler::plugin::plugin_registry;
    let reg = plugin_registry();
    let names: Vec<serde_json::Value> = reg.plugin_names().into_iter()
        .map(|n| serde_json::json!({ "name": n }))
        .collect();
    let body = serde_json::json!({ "count": names.len(), "plugins": names });
    (200, body.to_string())
}

// ── Logs ────────────────────────────────────────────────

pub fn route_log_level_get() -> (u16, String) {
    let level = tracing::level_filters::LevelFilter::current().to_string();
    let body = serde_json::json!({ "level": level });
    (200, body.to_string())
}

pub fn route_log_level_set(req: &ParsedRequest) -> (u16, String) {
    match serde_json::from_slice::<serde_json::Value>(&req.body) {
        Ok(v) => {
            if let Some(level_str) = v.get("level").and_then(|l| l.as_str()) {
                let valid = ["error", "warn", "info", "debug", "trace"];
                if !valid.contains(&level_str) {
                    return (400, err_json(&format!("无效日志级别: {}，有效值: {:?}", level_str, valid)));
                }
                info!("管理 API 修改日志级别: {}", level_str);
                (200, ok_json(&format!("日志级别已设为 {}", level_str)))
            } else {
                (400, err_json("缺少 level 字段"))
            }
        }
        Err(e) => (400, err_json(&format!("JSON 解析失败: {}", e))),
    }
}

// ── Metrics (Prometheus text/plain) ─────────────────────

pub fn route_metrics_text(ctx: &AdminContext) -> RouteResponse {
    let snap = ctx.metrics.snapshot();
    let text = crate::monitor::prometheus::format_metrics(&snap, None);
    RouteResponse::text(200, text)
}
