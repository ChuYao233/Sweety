//! Sites 端点：列表 / 详情 / 删除

use tracing::info;

use crate::admin_api::context::AdminContext;
use crate::admin_api::util::{ok_json, err_json};

pub fn route_sites_list(ctx: &AdminContext) -> (u16, String) {
    let sites = ctx.registry.all_sites();
    let list: Vec<serde_json::Value> = sites.iter().map(|s| {
        let upstream_names: Vec<&str> = s.upstreams.iter().map(|u| u.name.as_str()).collect();
        serde_json::json!({
            "name": s.name,
            "force_https": s.force_https,
            "websocket": s.websocket,
            "gzip": s.gzip,
            "fallback": s.fallback,
            "listen_tls": s.listen_tls,
            "location_count": s.locations.len(),
            "upstream_count": s.upstreams.len(),
            "upstream_names": upstream_names,
            "has_tls": s.tls.is_some(),
            "has_access_log": s.access_logger.is_some(),
            "has_proxy_cache": s.proxy_cache_arc.is_some(),
            "root": s.root.as_ref().map(|p| p.display().to_string()),
        })
    }).collect();
    let body = serde_json::json!({ "count": list.len(), "sites": list });
    (200, body.to_string())
}

pub fn route_site_detail(ctx: &AdminContext, name: &str) -> (u16, String) {
    let sites = ctx.registry.all_sites();
    let site = sites.iter().find(|s| s.name == name);
    match site {
        Some(s) => {
            let locations: Vec<serde_json::Value> = s.locations.iter().map(|loc| {
                serde_json::json!({
                    "path": loc.config.path,
                    "handler": format!("{:?}", loc.config.handler),
                    "upstream": loc.config.upstream,
                })
            }).collect();
            let upstreams: Vec<serde_json::Value> = s.upstreams.iter().map(|u| {
                serde_json::json!({
                    "name": u.name,
                    "strategy": format!("{:?}", u.strategy),
                    "node_count": u.nodes.len(),
                })
            }).collect();
            let body = serde_json::json!({
                "name": s.name,
                "force_https": s.force_https,
                "websocket": s.websocket,
                "gzip": s.gzip,
                "gzip_comp_level": s.gzip_comp_level,
                "fallback": s.fallback,
                "listen_tls": s.listen_tls,
                "root": s.root.as_ref().map(|p| p.display().to_string()),
                "index": s.index,
                "hsts": s.hsts.as_ref().map(|h| serde_json::json!({
                    "max_age": h.max_age,
                    "include_sub_domains": h.include_sub_domains,
                    "preload": h.preload,
                })),
                "locations": locations,
                "upstreams": upstreams,
                "error_pages": s.error_pages,
                "rewrite_count": s.rewrites.len(),
                "has_tls": s.tls.is_some(),
                "has_access_log": s.access_logger.is_some(),
                "has_proxy_cache": s.proxy_cache_arc.is_some(),
                "has_fastcgi": s.fastcgi.is_some(),
            });
            (200, body.to_string())
        }
        None => (404, err_json(&format!("站点 '{}' 不存在", name))),
    }
}

pub fn route_site_delete(ctx: &AdminContext, name: &str) -> (u16, String) {
    let sites = ctx.registry.all_sites();
    if !sites.iter().any(|s| s.name == name) {
        return (404, err_json(&format!("站点 '{}' 不存在", name)));
    }
    ctx.registry.remove_site(name);
    info!("管理 API 删除站点: {}", name);
    (200, ok_json(&format!("站点 '{}' 已删除", name)))
}
