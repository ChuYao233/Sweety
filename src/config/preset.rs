//! 内置站点预设模板
//!
//! 每个预设对应一类常见应用，自动生成合理的 location 规则，
//! 用户无需手写重复的伪静态、缓存、安全规则。
//!
//! # 使用方式
//!
//! ```toml
//! [[sites]]
//! preset      = "wordpress"
//! php_fastcgi = "/tmp/php-cgi-82.sock"
//! # ... 其他字段，无需写 [[sites.locations]]
//! ```
//!
//! # 规则优先级
//! - 若已手动配置 `[[sites.locations]]`，preset 不生效（手动优先）
//! - preset 展开后的 locations 可通过手动追加 locations 扩展

use serde::{Deserialize, Serialize};

use crate::config::model::{HandlerType, LocationConfig};

/// 内置站点预设类型
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SitePreset {
    /// WordPress / WooCommerce 站点
    WordPress,
    /// Laravel / Symfony / Slim 等 PHP MVC 框架
    Laravel,
    /// 纯静态站点（SPA / 文档站 / 前端构建产物）
    Static,
}

/// 为指定预设生成 location 规则列表
///
/// `has_php` - 站点是否配置了 FastCGI（影响是否生成 PHP location）
pub fn locations_for_preset(preset: &SitePreset, has_php: bool) -> Vec<LocationConfig> {
    match preset {
        SitePreset::WordPress => wordpress_locations(has_php),
        SitePreset::Laravel   => laravel_locations(has_php),
        SitePreset::Static    => static_site_locations(),
    }
}

// ─────────────────────────────────────────────
// WordPress 预设
// ─────────────────────────────────────────────

fn wordpress_locations(has_php: bool) -> Vec<LocationConfig> {
    let mut locs = vec![
        // 上传文件强缓存（30 天）
        LocationConfig {
            path: "^~ /wp-content/uploads/".into(),
            handler: HandlerType::Static,
            cache_control: Some("public, max-age=2592000".into()),
            ..Default::default()
        },
        // wp-content 静态资源（7 天）
        LocationConfig {
            path: "^~ /wp-content/".into(),
            handler: HandlerType::Static,
            cache_control: Some("public, max-age=604800".into()),
            ..Default::default()
        },
        // wp-includes 静态资源（7 天）
        LocationConfig {
            path: "^~ /wp-includes/".into(),
            handler: HandlerType::Static,
            cache_control: Some("public, max-age=604800".into()),
            ..Default::default()
        },
        // 禁止访问隐藏文件（.htaccess / .env 等）
        LocationConfig {
            path: "~ /\\.".into(),
            handler: HandlerType::Static,
            return_code: Some(403),
            ..Default::default()
        },
        // 禁止直接访问 xmlrpc.php（防暴力攻击）
        LocationConfig {
            path: "= /xmlrpc.php".into(),
            handler: HandlerType::Static,
            return_code: Some(403),
            ..Default::default()
        },
    ];

    if has_php {
        // PHP 文件交给 FastCGI 执行
        locs.push(LocationConfig {
            path: "~ \\.php$".into(),
            handler: HandlerType::Fastcgi,
            ..Default::default()
        });
        // WordPress 伪静态（根路由）
        locs.push(LocationConfig {
            path: "/".into(),
            handler: HandlerType::Fastcgi,
            try_files: vec!["$uri".into(), "$uri/".into(), "/index.php".into()],
            ..Default::default()
        });
    } else {
        // 无 PHP：纯静态兜底
        locs.push(LocationConfig {
            path: "/".into(),
            handler: HandlerType::Static,
            try_files: vec!["$uri".into(), "$uri/".into(), "/index.html".into()],
            ..Default::default()
        });
    }

    locs
}

// ─────────────────────────────────────────────
// Laravel 预设
// ─────────────────────────────────────────────

fn laravel_locations(has_php: bool) -> Vec<LocationConfig> {
    let mut locs = vec![
        // 禁止访问隐藏文件
        LocationConfig {
            path: "~ /\\.".into(),
            handler: HandlerType::Static,
            return_code: Some(403),
            ..Default::default()
        },
        // 静态资源强缓存（30 天）
        LocationConfig {
            path: "~ \\.(css|js|png|jpg|jpeg|gif|webp|ico|svg|woff2?|ttf)$".into(),
            handler: HandlerType::Static,
            cache_control: Some("public, max-age=2592000".into()),
            ..Default::default()
        },
    ];

    if has_php {
        locs.push(LocationConfig {
            path: "~ \\.php$".into(),
            handler: HandlerType::Fastcgi,
            ..Default::default()
        });
        // Laravel 伪静态（带 query_string）
        locs.push(LocationConfig {
            path: "/".into(),
            handler: HandlerType::Fastcgi,
            try_files: vec!["$uri".into(), "$uri/".into(), "/index.php?$query_string".into()],
            ..Default::default()
        });
    }

    locs
}

// ─────────────────────────────────────────────
// 纯静态站点预设
// ─────────────────────────────────────────────

fn static_site_locations() -> Vec<LocationConfig> {
    vec![
        // 禁止访问隐藏文件
        LocationConfig {
            path: "~ /\\.".into(),
            handler: HandlerType::Static,
            return_code: Some(403),
            ..Default::default()
        },
        // 静态资源强缓存（30 天）
        LocationConfig {
            path: "~ \\.(css|js|png|jpg|jpeg|gif|webp|ico|svg|woff2?|ttf|eot)$".into(),
            handler: HandlerType::Static,
            cache_control: Some("public, max-age=2592000".into()),
            ..Default::default()
        },
        // SPA / 文档站兜底：找不到文件时回退到 index.html
        LocationConfig {
            path: "/".into(),
            handler: HandlerType::Static,
            try_files: vec!["$uri".into(), "$uri/".into(), "/index.html".into()],
            ..Default::default()
        },
    ]
}
