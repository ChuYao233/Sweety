//! `sweety validate` —— 验证配置文件语法和 TLS 证书（等价 nginx -t）

use std::path::PathBuf;

use tracing::info;

use crate::util::{init_stderr_log, load_cfg_or_exit};

pub fn cmd_validate(config: &PathBuf) {
    init_stderr_log();
    let cfg = load_cfg_or_exit(config);
    info!("配置文件语法正确，共 {} 个站点", cfg.sites.len());

    let mut cert_ok = true;
    for site in &cfg.sites {
        if let Some(tls) = &site.tls {
            if !tls.acme {
                if let (Some(cert), Some(key)) = (tls.cert.as_ref(), tls.key.as_ref()) {
                    if let Err(e) = sweety_lib::server::tls::TlsManager::build_server_config(tls) {
                        eprintln!("[ERROR] 站点 '{}' TLS 证书验证失败: {:#}", site.name, e);
                        eprintln!("  cert: {}", cert.display());
                        eprintln!("  key:  {}", key.display());
                        cert_ok = false;
                    } else {
                        info!("站点 '{}' TLS 证书验证通过: {}", site.name, cert.display());
                    }
                }
                for (i, c) in tls.certs.iter().enumerate() {
                    let single_tls = sweety_lib::config::model::TlsConfig {
                        cert: Some(c.cert.clone()),
                        key:  Some(c.key.clone()),
                        certs: vec![],
                        acme: false,
                        ..tls.clone()
                    };
                    if let Err(e) = sweety_lib::server::tls::TlsManager::build_server_config(&single_tls) {
                        eprintln!("[ERROR] 站点 '{}' 第 {} 张证书验证失败: {:#}", site.name, i + 1, e);
                        eprintln!("  cert: {}", c.cert.display());
                        eprintln!("  key:  {}", c.key.display());
                        cert_ok = false;
                    } else {
                        info!("站点 '{}' 第 {} 张证书验证通过: {}", site.name, i + 1, c.cert.display());
                    }
                }
            }
        }
    }
    if !cert_ok {
        eprintln!("[ERROR] 配置测试失败：存在无效证书");
        std::process::exit(1);
    }
    info!("配置测试通过 (configuration test is successful)");
}
