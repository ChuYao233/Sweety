//! 配置文件加载模块
//! 支持 TOML / JSON / YAML 三种格式，通过文件扩展名自动识别

use std::path::Path;
use anyhow::{Context, Result, bail};

use super::model::AppConfig;

/// 根据文件扩展名自动选择解析器加载配置文件
pub fn load_config(path: &Path) -> Result<AppConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("读取配置文件失败: {}", path.display()))?;

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("toml")
        .to_lowercase();

    let cfg: AppConfig = match ext.as_str() {
        "toml" => toml::from_str(&content)
            .with_context(|| format!("TOML 解析失败: {}", path.display()))?,
        "json" => serde_json::from_str(&content)
            .with_context(|| format!("JSON 解析失败: {}", path.display()))?,
        "yaml" | "yml" => serde_yaml::from_str(&content)
            .with_context(|| format!("YAML 解析失败: {}", path.display()))?,
        other => bail!("不支持的配置文件格式: .{}", other),
    };

    validate_config(&cfg)?;
    Ok(cfg)
}

/// 配置校验：检查必填字段、端口合法性等
fn validate_config(cfg: &AppConfig) -> Result<()> {
    for site in &cfg.sites {
        // 校验站点名称非空
        if site.name.is_empty() {
            bail!("站点 name 不能为空");
        }
        // 校验至少有一个 server_name
        if site.server_name.is_empty() {
            bail!("站点 '{}' 的 server_name 列表不能为空", site.name);
        }
        // 校验上游引用合法性
        for loc in &site.locations {
            if loc.handler == crate::config::model::HandlerType::ReverseProxy {
                if loc.upstream.is_none() {
                    bail!(
                        "站点 '{}' 的 location '{}' 使用 reverse_proxy 但未指定 upstream",
                        site.name, loc.path
                    );
                }
                let up_name = loc.upstream.as_deref().unwrap();
                if !site.upstreams.iter().any(|u| u.name == up_name) {
                    bail!(
                        "站点 '{}' 的 location '{}' 引用了不存在的上游组 '{}'",
                        site.name, loc.path, up_name
                    );
                }
            }
        }
        // 校验 TLS：手动证书模式下 cert 和 key 都需指定
        if let Some(tls) = &site.tls {
            if !tls.acme {
                if tls.cert.is_none() || tls.key.is_none() {
                    bail!(
                        "站点 '{}' 的 TLS 配置：非 ACME 模式必须同时指定 cert 和 key",
                        site.name
                    );
                }
            }
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
    use tempfile::NamedTempFile;

    fn write_temp(content: &str, ext: &str) -> NamedTempFile {
        let mut f = tempfile::Builder::new()
            .suffix(&format!(".{}", ext))
            .tempfile()
            .unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn test_load_toml() {
        let toml = r#"
            [[sites]]
            name = "demo"
            server_name = ["localhost"]
            root = "/tmp/www"
        "#;
        let f = write_temp(toml, "toml");
        let cfg = load_config(f.path()).unwrap();
        assert_eq!(cfg.sites[0].name, "demo");
    }

    #[test]
    fn test_load_json() {
        let json = r#"{"sites":[{"name":"demo","server_name":["localhost"],"root":"/tmp/www"}]}"#;
        let f = write_temp(json, "json");
        let cfg = load_config(f.path()).unwrap();
        assert_eq!(cfg.sites[0].name, "demo");
    }

    #[test]
    fn test_load_yaml() {
        let yaml = "sites:\n  - name: demo\n    server_name:\n      - localhost\n    root: /tmp/www\n";
        let f = write_temp(yaml, "yaml");
        let cfg = load_config(f.path()).unwrap();
        assert_eq!(cfg.sites[0].name, "demo");
    }

    #[test]
    fn test_invalid_extension() {
        let f = write_temp("", "xml");
        assert!(load_config(f.path()).is_err());
    }

    #[test]
    fn test_validate_empty_name() {
        let toml = r#"[[sites]]
            name = ""
            server_name = ["localhost"]
        "#;
        let f = write_temp(toml, "toml");
        assert!(load_config(f.path()).is_err());
    }

    #[test]
    fn test_validate_missing_upstream_ref() {
        let toml = r#"
            [[sites]]
            name = "demo"
            server_name = ["localhost"]
            [[sites.locations]]
            path = "/"
            handler = "reverse_proxy"
            upstream = "nonexistent"
        "#;
        let f = write_temp(toml, "toml");
        assert!(load_config(f.path()).is_err());
    }
}
