//! 路由分发

use super::context::AdminContext;
use super::server::{ParsedRequest, RouteResponse};
use super::util::err_json;
use super::doc::build_api_doc;
use super::handlers::{system, config, sites, upstreams, runtime};

pub async fn route(req: &ParsedRequest, ctx: &AdminContext) -> RouteResponse {
    let m = req.method.as_str();
    let p = req.path.as_str();

    // ═══════════════════════════════════════════════════════════════
    // Caddy-equivalent 端点（配置树 CRUD + 适配器 + 运行时状态）
    // ═══════════════════════════════════════════════════════════════

    // ── POST /load — 整体热加载配置（失败自动回滚） ──────────────
    if (m, p) == ("POST", "/load") {
        return config::route_load(ctx, req).into();
    }

    // ── /config/save — 显式保存运行配置到磁盘 ────────────────────
    if (m, p) == ("POST", "/config/save") {
        return config::route_config_save(ctx).into();
    }

    // ── /config/reload — 从磁盘热重载配置 ────────────────────────
    if (m, p) == ("POST", "/config/reload") {
        return config::route_config_reload(ctx).into();
    }

    // ── /config/test — 验证磁盘配置文件 ──────────────────────────
    if (m, p) == ("POST", "/config/test") {
        return config::route_config_test(ctx).into();
    }

    // ── /config/[path] — 配置树 CRUD ─────────────────────────────
    if p == "/config" || p == "/config/" || p.starts_with("/config/") {
        return config::route_config_tree(ctx, req).into();
    }

    // ── /id/:id[/path] — @id 配置节点直达 ────────────────────────
    if p.starts_with("/id/") {
        return config::route_id_lookup(ctx, req).into();
    }

    // ── POST /adapt — TOML → JSON 配置适配 ───────────────────────
    if (m, p) == ("POST", "/adapt") {
        return config::route_adapt(req).into();
    }

    // ── GET /reverse_proxy/upstreams — Caddy 兼容上游状态 ────────
    if (m, p) == ("GET", "/reverse_proxy/upstreams") {
        return upstreams::route_caddy_upstreams(ctx).into();
    }

    // ── GET /metrics — Prometheus text/plain 原生格式 ─────────────
    if (m, p) == ("GET", "/metrics") {
        return runtime::route_metrics_text(ctx);
    }

    // ═══════════════════════════════════════════════════════════════
    // Sweety 扩展端点
    // ═══════════════════════════════════════════════════════════════

    // ── System ──────────────────────────────────────────
    if matches!((m, p), ("GET", "/api/health") | ("GET", "/health")) {
        return system::route_health().into();
    }
    if (m, p) == ("GET", "/api/version") { return system::route_version().into(); }
    if (m, p) == ("GET", "/api/system")  { return system::route_system(ctx).into(); }
    if (m, p) == ("GET", "/api/doc")     { return RouteResponse::json(200, build_api_doc().to_string()); }
    if (m, p) == ("POST", "/api/stop")   { return system::route_stop().into(); }
    if (m, p) == ("GET", "/api/debug")   { return system::route_debug(ctx).into(); }

    // ── Metrics ─────────────────────────────────────────
    if (m, p) == ("GET", "/api/stats") { return system::route_stats(ctx).into(); }

    // ── Sites ───────────────────────────────────────────
    if (m, p) == ("GET", "/api/sites") { return sites::route_sites_list(ctx).into(); }
    if m == "GET" && p.starts_with("/api/sites/") {
        let name = &p["/api/sites/".len()..];
        if !name.is_empty() && !name.contains('/') {
            return sites::route_site_detail(ctx, name).into();
        }
    }
    if m == "DELETE" && p.starts_with("/api/sites/") {
        let name = &p["/api/sites/".len()..];
        if !name.is_empty() && !name.contains('/') {
            return sites::route_site_delete(ctx, name).into();
        }
    }

    // ── Upstreams ───────────────────────────────────────
    if (m, p) == ("GET", "/api/upstreams") { return upstreams::route_upstreams_list(ctx).into(); }
    if m == "GET" && p.starts_with("/api/upstreams/") {
        let rest = &p["/api/upstreams/".len()..];
        if !rest.is_empty() && !rest.contains('/') {
            return upstreams::route_upstream_detail(ctx, rest).into();
        }
    }
    if (m == "POST" || m == "PUT") && p.starts_with("/api/upstreams/") {
        if let Some(result) = upstreams::route_upstream_node_action(ctx, req).await {
            return result.into();
        }
    }

    // ── Certificates ────────────────────────────────────
    if (m, p) == ("GET", "/api/certs")               { return runtime::route_certs_list(ctx).into(); }
    if (m, p) == ("POST", "/api/certs/reload")       { return runtime::route_certs_reload(ctx).into(); }
    if m == "POST" && (p == "/api/certs/acme/renew" || p.starts_with("/api/certs/acme/renew?")) {
        return runtime::route_certs_acme_renew(ctx, req).into();
    }

    // ── Cache ───────────────────────────────────────────
    if (m, p) == ("GET", "/api/cache/stats")    { return runtime::route_cache_stats(ctx).into(); }
    if (m, p) == ("POST", "/api/cache/purge")   { return runtime::route_cache_purge(ctx).into(); }

    // ── Connections ─────────────────────────────────────
    if (m, p) == ("GET", "/api/connections")     { return runtime::route_connections(ctx).into(); }

    // ── Plugins ─────────────────────────────────────────
    if (m, p) == ("GET", "/api/plugins")        { return runtime::route_plugins().into(); }

    // ── Logs ────────────────────────────────────────────
    if (m, p) == ("GET", "/api/logs/level")     { return runtime::route_log_level_get().into(); }
    if (m, p) == ("PUT", "/api/logs/level")     { return runtime::route_log_level_set(req).into(); }

    // 404
    RouteResponse::json(404, err_json(&format!("Not Found: {} {}", m, p)))
}
