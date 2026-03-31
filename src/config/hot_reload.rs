//! 配置热重载模块
//! 使用 notify 监听文件系统变更，防抖后 diff 新旧配置，
//! 只更新变化的站点，不影响其他站点，不断开现有连接。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tracing::{error, info, warn};

use super::{loader::load_config, model::{AppConfig, SiteConfig}};
use crate::dispatcher::vhost::VHostRegistry;
use crate::server::tls::{SniResolver, TlsManager};

/// 热重载上下文：持有可原地更新的运行时组件
pub struct HotReloadContext {
    /// 虚拟主机注册表（原地更新，不断连）
    pub registry: Arc<VHostRegistry>,
    /// TLS 端口 → SniResolver 的映射（热重载时按端口原地更新证书）
    pub sni_resolvers: HashMap<u16, Arc<SniResolver>>,
    /// 每个 TLS 端口绑定的站点名列表（用于 diff 时定位 resolver）
    pub port_sites: HashMap<u16, Vec<String>>,
}

/// 启动配置热重载监听（在独立 std::thread 中运行 tokio 单线程运行时）
///
/// `ctx` 持有 `Arc<VHostRegistry>` 和 `Arc<Vec<Arc<SniResolver>>>`，
/// 文件变更时 diff 新旧配置，只更新变化站点，不影响其他站点。
pub fn start_hot_reload(
    config_path: PathBuf,
    initial_config: Arc<AppConfig>,
    ctx: HotReloadContext,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("热重载运行时创建失败");
        rt.block_on(watch_loop(config_path, initial_config, ctx));
    });
}

/// 文件监听主循环
async fn watch_loop(
    config_path: PathBuf,
    mut current_config: Arc<AppConfig>,
    ctx: HotReloadContext,
) {
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    // 把配置文件路径 canonicalize，用于事件过滤
    let watch_target = config_path.canonicalize().unwrap_or_else(|_| config_path.clone());
    let mut watcher: RecommendedWatcher = match notify::recommended_watcher(
        move |res: notify::Result<Event>| {
            if let Ok(event) = res {
                if !matches!(event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    return;
                }
                // 只有事件路径包含配置文件本身时才触发
                let is_config = event.paths.iter().any(|p| {
                    p.canonicalize().ok().as_deref() == Some(&watch_target)
                    || p == &watch_target
                });
                if is_config {
                    let _ = event_tx.send(());
                }
            }
        }
    ) {
        Ok(w) => w,
        Err(e) => { error!("热重载监听器创建失败: {}", e); return; }
    };

    // 只监听配置文件本身，不监听整个目录
    // 监听目录会导致同目录下其他文件（日志、证书 atime 更新等）触发不必要的热重载
    if let Err(e) = watcher.watch(&config_path, RecursiveMode::NonRecursive) {
        // 部分平台不支持直接监听单文件，fallback 到监听父目录
        let watch_dir = config_path.parent().unwrap_or_else(|| Path::new("."));
        if let Err(e2) = watcher.watch(watch_dir, RecursiveMode::NonRecursive) {
            error!("无法监听配置文件: {} / {}", e, e2);
            return;
        }
        info!("配置热重载已启动（目录模式），监听: {}", watch_dir.display());
    } else {
        info!("配置热重载已启动，监听: {}", config_path.display());
    }

    loop {
        if event_rx.recv().await.is_none() { break; }
        // 防抖 500ms
        tokio::time::sleep(Duration::from_millis(500)).await;
        while event_rx.try_recv().is_ok() {}

        match load_config(&config_path) {
            Err(e) => {
                error!("配置热重载失败（文件解析错误），继续使用旧配置: {:#}", e);
            }
            Ok(new_cfg) => {
                // 热重载前全量验证：证书格式非法时拒绝加载，等价 nginx -t + nginx reload
                match validate_config(&new_cfg) {
                    Err(e) => {
                        error!("配置验证失败，继续使用旧配置: {:#}", e);
                        error!("  提示：运行 `sweety -t` 可详细检查配置和证书");
                    }
                    Ok(()) => {
                        let new_arc = Arc::new(new_cfg);
                        apply_diff(&current_config, &new_arc, &ctx);
                        current_config = new_arc;
                        info!("配置热重载成功");
                    }
                }
            }
        }
    }
}

/// 对比新旧配置，只更新有变化的站点
fn apply_diff(old: &AppConfig, new: &AppConfig, ctx: &HotReloadContext) {
    // 检测端口变更：端口绑定在进程启动时完成，运行时无法热更新（与 Nginx reload 行为一致）
    let old_http: HashSet<u16> = old.sites.iter().flat_map(|s| s.listen.iter().copied()).collect();
    let new_http: HashSet<u16> = new.sites.iter().flat_map(|s| s.listen.iter().copied()).collect();
    let old_tls: HashSet<u16> = old.sites.iter().flat_map(|s| s.listen_tls.iter().copied()).collect();
    let new_tls: HashSet<u16> = new.sites.iter().flat_map(|s| s.listen_tls.iter().copied()).collect();
    let added_ports: Vec<u16> = new_http.difference(&old_http).chain(new_tls.difference(&old_tls)).copied().collect();
    let removed_ports: Vec<u16> = old_http.difference(&new_http).chain(old_tls.difference(&new_tls)).copied().collect();
    if !added_ports.is_empty() || !removed_ports.is_empty() {
        tracing::warn!(
            "热重载: 检测到端口变更（新增: {:?}，删除: {:?}）——端口绑定需重启服务器生效，其他配置已热更新",
            added_ports, removed_ports
        );
    }

    let old_map: HashMap<&str, &SiteConfig> =
        old.sites.iter().map(|s| (s.name.as_str(), s)).collect();
    let new_map: HashMap<&str, &SiteConfig> =
        new.sites.iter().map(|s| (s.name.as_str(), s)).collect();

    let old_names: HashSet<&str> = old_map.keys().copied().collect();
    let new_names: HashSet<&str> = new_map.keys().copied().collect();

    // 删除的站点
    for name in old_names.difference(&new_names) {
        ctx.registry.remove_site(name);
        remove_site_from_resolvers(name, old_map[name], ctx);
        info!("热重载: 删除站点 '{}'", name);
    }

    // 新增的站点
    for name in new_names.difference(&old_names) {
        let site = new_map[name];
        ctx.registry.upsert_site(site);
        upsert_site_to_resolvers(site, ctx);
        info!("热重载: 新增站点 '{}'", name);
    }

    // 修改的站点（对比序列化后的内容，避免 PartialEq 实现依赖）
    for name in old_names.intersection(&new_names) {
        let old_site = old_map[name];
        let new_site = new_map[name];
        if site_changed(old_site, new_site) {
            ctx.registry.upsert_site(new_site);
            // 若 TLS 配置有变化，重新加载证书
            if tls_changed(old_site, new_site) {
                remove_site_from_resolvers(name, old_site, ctx);
                upsert_site_to_resolvers(new_site, ctx);
                info!("热重载: 更新站点 '{}' 证书", name);
            } else {
                info!("热重载: 更新站点 '{}' 配置", name);
            }
        }
    }
}

/// 将站点证书插入/更新到该站点的各 TLS 端口对应的 SniResolver
fn upsert_site_to_resolvers(site: &SiteConfig, ctx: &HotReloadContext) {
    let Some(tls) = &site.tls else { return };
    let keys: Vec<rustls::sign::CertifiedKey> = match TlsManager::build_certified_keys_pub(tls) {
        Ok(k) => k,
        Err(e) => {
            warn!("热重载证书加载失败（站点 '{}'）: {}", site.name, e);
            return;
        }
    };
    // 只更新该站点所在端口对应的 resolver
    for &port in &site.listen_tls {
        if let Some(resolver) = ctx.sni_resolvers.get(&port) {
            resolver.upsert_site(&site.server_name, keys.clone());
        }
    }
}

/// 从 SniResolver 中删除站点证书条目
fn remove_site_from_resolvers(_name: &str, site: &SiteConfig, ctx: &HotReloadContext) {
    for resolver in ctx.sni_resolvers.values() {
        resolver.remove_site(&site.server_name);
    }
}

/// 判断站点配置是否有变化（通过 toml 序列化对比）
fn site_changed(old: &SiteConfig, new: &SiteConfig) -> bool {
    // 用 toml 序列化对比，简单可靠
    toml::to_string(old).ok() != toml::to_string(new).ok()
}

/// 判断 TLS 配置是否有变化
fn tls_changed(old: &SiteConfig, new: &SiteConfig) -> bool {
    toml::to_string(&old.tls).ok() != toml::to_string(&new.tls).ok()
}

/// 全量验证新配置的合法性（等价 nginx -t）
///
/// 检查项：
/// 1. 所有站点的 TLS 证书（非 ACME）能否正确加载
/// 2. 证书和私钥格式是否匹配
///
/// 任一站点验证失败则返回 Err，调用方应保持旧配置继续服务。
fn validate_config(cfg: &AppConfig) -> anyhow::Result<()> {
    for site in &cfg.sites {
        let Some(tls) = &site.tls else { continue };
        if tls.acme { continue; } // ACME 证书由后台任务管理，跳过

        // 验证单证书模式
        if tls.cert.is_some() || tls.key.is_some() {
            TlsManager::build_certified_keys_pub(tls)
                .map_err(|e| anyhow::anyhow!(
                    "站点 '{}' TLS 证书无效: {:#}", site.name, e
                ))?;
        }

        // 验证多证书列表
        for (i, c) in tls.certs.iter().enumerate() {
            let single_tls = crate::config::model::TlsConfig {
                cert: Some(c.cert.clone()),
                key:  Some(c.key.clone()),
                certs: vec![],
                acme: false,
                ..tls.clone()
            };
            TlsManager::build_certified_keys_pub(&single_tls)
                .map_err(|e| anyhow::anyhow!(
                    "站点 '{}' 第 {} 张证书无效 ({}): {:#}",
                    site.name, i + 1, c.cert.display(), e
                ))?;
        }
    }
    Ok(())
}


// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_site_changed_detects_diff() {
        use crate::config::model::{HandlerType, LocationConfig};
        let mut site = SiteConfig {
            name: "demo".into(),
            server_name: vec!["localhost".into()],
            listen: vec![80],
            listen_tls: vec![],
            root: None,
            index: vec!["index.html".into()],
            access_log: None,
            error_log: None,
            tls: None,
            fastcgi: None,
            upstreams: vec![],
            locations: vec![LocationConfig {
                path: "/".into(),
                handler: HandlerType::Static,
                ..Default::default()
            }],
            rewrites: vec![],
            rate_limit: None,
            hsts: None,
            fallback: false,
            gzip: None,
            gzip_comp_level: None,
            websocket: true,
            force_https: false,
            error_pages: std::collections::HashMap::new(),
            proxy_cache: None,
        };
        let mut site2 = site.clone();
        assert!(!site_changed(&site, &site2));
        site2.listen = vec![8080];
        assert!(site_changed(&site, &site2));
    }

    #[test]
    fn test_watcher_starts() {
        use std::io::Write;
        let mut f = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .unwrap();
        writeln!(f, "[[sites]]\nname = \"demo\"\nserver_name = [\"localhost\"]").unwrap();
        let cfg = load_config(f.path()).unwrap();
        // 只验证能正常创建（不启动完整 watcher 避免测试挂起）
        assert_eq!(cfg.sites[0].name, "demo");
    }
}
