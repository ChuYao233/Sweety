//! TLS 管理模块
//! 负责：Rustls 配置构建、证书加载、ACME 自动续期调度
//! 后续版本完整实现，当前提供骨架与核心数据结构

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use rustls::ServerConfig;
use tracing::info;

use crate::config::model::TlsConfig;

/// 为单个站点构建 Rustls ServerConfig
///
/// 支持两种模式：
/// 1. 手动证书（cert + key 文件路径）
/// 2. ACME 自动证书（由 acme 模块负责申请和续期）
pub async fn build_tls_config(tls_cfg: &TlsConfig, domain: &str) -> Result<Arc<ServerConfig>> {
    if tls_cfg.acme {
        build_acme_config(domain, tls_cfg.acme_email.as_deref()).await
    } else {
        let cert_path = tls_cfg
            .cert
            .as_ref()
            .context("TLS 配置缺少 cert 路径")?;
        let key_path = tls_cfg
            .key
            .as_ref()
            .context("TLS 配置缺少 key 路径")?;
        build_manual_config(cert_path, key_path)
    }
}

/// 从 PEM 文件加载证书和私钥，构建 Rustls ServerConfig
fn build_manual_config(cert_path: &Path, key_path: &Path) -> Result<Arc<ServerConfig>> {
    // 读取证书链
    let cert_file = std::fs::File::open(cert_path)
        .with_context(|| format!("打开证书文件失败: {}", cert_path.display()))?;
    let mut cert_reader = std::io::BufReader::new(cert_file);
    let certs: Vec<rustls::pki_types::CertificateDer> =
        rustls_pemfile::certs(&mut cert_reader)
            .collect::<Result<_, _>>()
            .with_context(|| format!("解析证书 PEM 失败: {}", cert_path.display()))?;

    // 读取私钥
    let key_file = std::fs::File::open(key_path)
        .with_context(|| format!("打开私钥文件失败: {}", key_path.display()))?;
    let mut key_reader = std::io::BufReader::new(key_file);
    let key = rustls_pemfile::private_key(&mut key_reader)
        .with_context(|| format!("解析私钥 PEM 失败: {}", key_path.display()))?
        .context("私钥文件为空")?;

    // 构建 ServerConfig（支持 HTTP/2 ALPN 协商）
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("Rustls ServerConfig 构建失败")?;

    info!("TLS 手动证书加载成功: {}", cert_path.display());
    Ok(Arc::new(config))
}

/// 通过 ACME（Let's Encrypt）申请证书并构建 ServerConfig
///
/// 当前为骨架实现，完整 ACME 流程在 v0.4 迭代中实现：
/// - HTTP-01 Challenge
/// - 证书存储到磁盘
/// - 定时续期（到期前 30 天自动续期）
#[allow(unused_variables)]
async fn build_acme_config(domain: &str, email: Option<&str>) -> Result<Arc<ServerConfig>> {
    // TODO（v0.4）：接入 instant-acme 完整实现
    // 1. 创建 ACME account（或加载已有 account key）
    // 2. 发起 order
    // 3. 完成 HTTP-01 challenge
    // 4. 下载证书链
    // 5. 存储到 ~/.config/sweety/certs/<domain>/
    // 6. 调用 build_manual_config 加载证书
    // 7. 启动定时续期 task

    anyhow::bail!(
        "ACME 自动证书功能尚未完整实现（规划于 v0.4），域名: {}。\
        请暂时使用手动证书（cert + key 配置项）。",
        domain
    )
}

/// ACME 续期调度器（后台 task）
///
/// 每小时检查一次所有证书的有效期，到期前 30 天自动触发续期
pub async fn acme_renewal_task() {
    loop {
        // TODO（v0.4）：检查所有已知证书有效期，触发续期
        tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
    }
}
