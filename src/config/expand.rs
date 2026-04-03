//! 配置语法糖展开
//!
//! 将高层次简写字段展开为底层完整配置，
//! 在 `load_config` 解析完成后、`validate_config` 执行前调用。
//!
//! # 展开规则
//! - `php_fastcgi = "/tmp/php.sock"` → 等同完整 `[sites.fastcgi]` 块
//! - `preset = "wordpress"` → 自动生成 location 规则（仅在 locations 为空时）
//!
//! 手动配置始终优先：若用户已写 `[sites.fastcgi]` 或 `[[sites.locations]]`，
//! 对应语法糖不会覆盖。

use crate::config::model::{AppConfig, FastCgiConfig, SiteConfig, TlsConfig};
use crate::config::preset::locations_for_preset;

/// 展开 AppConfig 中所有站点的语法糖字段
pub fn expand_config(cfg: &mut AppConfig) {
    for site in &mut cfg.sites {
        expand_tls_auto(site);
        expand_php_fastcgi(site);
        expand_preset(site);
    }
}

/// `acme_email` 快捷字段展开 → 自动生成 ACME TlsConfig
///
/// 条件：`listen_tls` 非空 && `tls` 为 None && `acme_email` 有值
fn expand_tls_auto(site: &mut SiteConfig) {
    if site.tls.is_some() || site.listen_tls.is_empty() {
        return;
    }
    if let Some(email) = site.acme_email.as_deref() {
        site.tls = Some(TlsConfig {
            acme: true,
            acme_email: Some(email.to_string()),
            ..TlsConfig::default()
        });
    }
}

/// `php_fastcgi` 快捷字段展开
///
/// `php_fastcgi = "/tmp/php.sock"` → `site.fastcgi = Some(FastCgiConfig::from_addr(...))`
fn expand_php_fastcgi(site: &mut SiteConfig) {
    // 已有精细配置时跳过，手动优先
    if site.fastcgi.is_some() {
        return;
    }
    if let Some(addr) = site.php_fastcgi.as_deref() {
        site.fastcgi = Some(FastCgiConfig::from_addr(addr));
    }
}

/// `preset` 展开 → 自动生成 location 规则
///
/// 仅在 `site.locations` 为空时生效，保证手动规则优先。
fn expand_preset(site: &mut SiteConfig) {
    let preset = match &site.preset {
        Some(p) => p.clone(),
        None    => return,
    };
    // 已手动配置 locations，不覆盖
    if !site.locations.is_empty() {
        return;
    }
    // expand_php_fastcgi 已在此之前执行，site.fastcgi 已被设置（若有 php_fastcgi）
    let has_php = site.fastcgi.is_some();
    site.locations = locations_for_preset(&preset, has_php);
}
