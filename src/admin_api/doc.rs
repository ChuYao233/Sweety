//! API 文档生成

/// 生成 API 文档 JSON（给 --api-doc 和 /api/doc 共用）
pub fn build_api_doc() -> serde_json::Value {
    serde_json::json!({
        "name": "Sweety Admin API",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "对标 Caddy Admin API 全部功能，并扩展更多端点",
        "auth": {
            "type": "Bearer",
            "header": "Authorization",
            "description": "设置 global.admin_token 开启鉴权（为空则不鉴权）",
            "no_auth_paths": ["/api/health", "/health", "/api/version", "/api/doc", "/metrics"]
        },
        "groups": {
            "caddy_config": {
                "description": "配置树 CRUD（对标 Caddy /config/）— 默认仅改运行时，?save=true 持久化到磁盘",
                "endpoints": [
                    ep("POST", "/load", "整体热加载 JSON 配置（失败自动回滚）", true),
                    ep("GET", "/config/[path]", "读取运行中配置子树", true),
                    ep("POST", "/config/[path]", "创建/替换对象 | 追加数组元素", true),
                    ep("PUT", "/config/[path]", "数组按索引插入 | 严格创建（已有报错）", true),
                    ep("PATCH", "/config/[path]", "仅替换已有值", true),
                    ep("DELETE", "/config/[path]", "删除配置节点（/config/ = 清空不退出）", true),
                    ep("POST", "/config/save", "显式保存运行配置到磁盘（TOML）", true),
                    ep("POST", "/config/reload", "从磁盘热重载配置", true),
                    ep("POST", "/config/test", "验证磁盘配置文件语法", true),
                ]
            },
            "caddy_id": {
                "description": "@id 配置节点直达（对标 Caddy /id/）",
                "endpoints": [
                    ep("GET", "/id/:id", "通过 @id 直接访问配置节点", true),
                    ep("GET", "/id/:id/[path]", "通过 @id + 子路径访问配置", true),
                ]
            },
            "caddy_adapt": {
                "description": "配置适配器（对标 Caddy /adapt）",
                "endpoints": [
                    ep("POST", "/adapt", "TOML → JSON 配置转换", true),
                ]
            },
            "caddy_runtime": {
                "description": "运行时状态（对标 Caddy）",
                "endpoints": [
                    ep("GET", "/reverse_proxy/upstreams", "上游状态（Caddy 兼容格式）", true),
                    ep("GET", "/metrics", "Prometheus text/plain 指标", false),
                    ep("POST", "/api/stop", "优雅停机", true),
                ]
            },
            "system": {
                "description": "系统管理",
                "endpoints": [
                    ep("GET", "/api/health", "健康检查", false),
                    ep("GET", "/api/version", "版本 + 构建信息", false),
                    ep("GET", "/api/system", "系统信息（uptime/workers/memory）", true),
                    ep("GET", "/api/doc", "API 文档", false),
                    ep("GET", "/api/debug", "运行时调试信息", true),
                ]
            },
            "metrics": {
                "description": "指标统计",
                "endpoints": [
                    ep("GET", "/api/stats", "全局统计快照（JSON）", true),
                ]
            },
            "sites": {
                "description": "站点管理",
                "endpoints": [
                    ep("GET", "/api/sites", "站点列表 + 摘要", true),
                    ep("GET", "/api/sites/:name", "单个站点详情", true),
                    ep("DELETE", "/api/sites/:name", "删除站点（热生效）", true),
                ]
            },
            "upstreams": {
                "description": "上游管理",
                "endpoints": [
                    ep("GET", "/api/upstreams", "所有上游组 + 节点状态", true),
                    ep("GET", "/api/upstreams/:name", "单个上游组详情", true),
                    ep("POST", "/api/upstreams/:name/nodes/:addr/enable", "启用节点", true),
                    ep("POST", "/api/upstreams/:name/nodes/:addr/disable", "禁用节点", true),
                    ep("PUT", "/api/upstreams/:name/nodes/:addr/weight", "修改节点权重（{\"weight\":5}）", true),
                ]
            },
            "certs": {
                "description": "证书管理",
                "endpoints": [
                    ep("GET", "/api/certs", "TLS 证书列表", true),
                    ep("POST", "/api/certs/reload", "重新加载磁盘证书", true),
                    ep("POST", "/api/certs/acme/renew", "立即触发 ACME 证书续期（?site=name 指定站点）", true),
                ]
            },
            "cache": {
                "description": "缓存管理",
                "endpoints": [
                    ep("GET", "/api/cache/stats", "缓存统计", true),
                    ep("POST", "/api/cache/purge", "清除所有缓存", true),
                ]
            },
            "connections": {
                "description": "连接管理",
                "endpoints": [
                    ep("GET", "/api/connections", "活跃连接数 + 连接池状态", true),
                ]
            },
            "plugins": {
                "description": "插件管理",
                "endpoints": [
                    ep("GET", "/api/plugins", "已注册插件列表", true),
                ]
            },
            "logs": {
                "description": "日志管理",
                "endpoints": [
                    ep("GET", "/api/logs/level", "当前日志级别", true),
                    ep("PUT", "/api/logs/level", "修改日志级别（{\"level\":\"debug\"}）", true),
                ]
            },
        }
    })
}

fn ep(method: &str, path: &str, desc: &str, auth: bool) -> serde_json::Value {
    serde_json::json!({ "method": method, "path": path, "description": desc, "auth_required": auth })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_doc_structure() {
        let doc = build_api_doc();
        assert!(doc["groups"]["system"]["endpoints"].is_array());
        assert!(doc["groups"]["caddy_config"]["endpoints"].is_array());
        assert!(doc["groups"]["upstreams"]["endpoints"].is_array());
        assert!(doc["groups"]["logs"]["endpoints"].is_array());
    }
}
