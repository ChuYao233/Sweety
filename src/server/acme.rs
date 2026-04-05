//! ACME（自动证书管理环境）模块
//! 负责：HTTP-01 / DNS-01 证书申请、自动续期、热重载
//!
//! 支持提供商：Let's Encrypt、ZeroSSL、Buypass 及任意自定义 ACME 目录 URL

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tracing::{error, info, warn};

use crate::config::model::AppConfig;
use super::tls::SniResolver;

// ─────────────────────────────────────────────
// ACME 提供商目录 URL
// ─────────────────────────────────────────────

const LETS_ENCRYPT_PROD:    &str = "https://acme-v02.api.letsencrypt.org/directory";
const LETS_ENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";
const ZEROSSL:              &str = "https://acme.zerossl.com/v2/DV90";
const BUYPASS:              &str = "https://api.buypass.com/acme/directory";
const LITESSL:              &str = "https://acme.litessl.com/directory";

/// 全局 HTTP-01 challenge token 存储（token → key_authorization）
/// 由 ACME 申请流程写入，HTTP handler 读取并响应 Let's Encrypt 验证请求
pub static ACME_HTTP01_TOKENS: std::sync::LazyLock<dashmap::DashMap<String, String>> =
    std::sync::LazyLock::new(dashmap::DashMap::new);

// ─────────────────────────────────────────────
// 路径辅助函数
// ─────────────────────────────────────────────

pub(super) fn acme_cache_dir() -> PathBuf {
    dirs_next::config_dir()
        .unwrap_or_else(|| PathBuf::from("/etc"))
        .join("sweety")
        .join("acme")
}

pub fn acme_cert_path(domain: &str) -> PathBuf {
    acme_cache_dir().join(format!("{}.crt", domain))
}

pub fn acme_key_path(domain: &str) -> PathBuf {
    acme_cache_dir().join(format!("{}.key", domain))
}

/// 从域名列表中选取主域名（用于证书文件命名）
/// 优先选第一个非通配符域名，否则取第一个
pub fn primary_domain(server_names: &[String]) -> &str {
    server_names.iter()
        .find(|d| !d.starts_with("*."))
        .or_else(|| server_names.first())
        .map(|s| s.as_str())
        .unwrap_or("localhost")
}

// ─────────────────────────────────────────────
// 公开接口：证书续期后台循环
// ─────────────────────────────────────────────

/// ACME 证书自动申请与续期后台循环
///
/// - HTTP-01 challenge，需要 80 端口可达；DNS-01 支持通配符证书
/// - 每 12 小时检查一次
/// - 到期前 `acme_renew_days_before` 天自动续期（解析真实证书到期日）
/// - 续期成功后通知 `sni_resolvers` 热重载证书，不重启服务器
pub async fn acme_renewal_loop(
    cfg: Arc<AppConfig>,
    sni_resolvers: HashMap<u16, Arc<SniResolver>>,
) {
    loop {
        for site in &cfg.sites {
            let Some(tls) = &site.tls else { continue };
            if !tls.acme { continue }

            let email = match &tls.acme_email {
                Some(e) => e.clone(),
                None => {
                    warn!("站点 '{}' 启用了 ACME 但未配置 acme_email，跳过", site.name);
                    continue;
                }
            };
            let renew_days = tls.acme_renew_days_before;
            let use_dns01 = tls.acme_challenge.as_str() == "dns01";

            // 过滤可用域名（HTTP-01 不支持通配符）
            let domains: Vec<String> = site.server_name.iter()
                .filter(|d| {
                    if !use_dns01 && d.starts_with("*.") {
                        warn!("ACME HTTP-01 不支持通配符证书 '{}'，请改用 acme_challenge = \"dns01\"", d);
                        false
                    } else {
                        true
                    }
                })
                .cloned()
                .collect();
            if domains.is_empty() { continue; }

            // 用主域名定位证书文件（与 tls.rs build_server_config 一致）
            let primary = primary_domain(&domains);
            let cert_path = acme_cert_path(primary);
            let key_path  = acme_key_path(primary);

            // 检查是否需要续期
            if cert_path.exists() && !cert_needs_renewal(&cert_path, renew_days) {
                continue;
            }

            info!("开始为站点 '{}' 申请/续期 ACME SAN 证书（{} 个域名: {}）",
                site.name, domains.len(), domains.join(", "));

            let result = if use_dns01 {
                match &tls.dns_provider {
                    Some(provider) => {
                        request_acme_cert_dns01(&domains, &email, &tls.acme_provider, provider).await
                    }
                    None => {
                        Err(anyhow::anyhow!(
                            "站点 '{}' 配置了 acme_challenge=dns01 但没有配置 dns_provider", site.name
                        ))
                    }
                }
            } else {
                request_acme_cert(&domains, &email, &tls.acme_provider).await
            };

            match result {
                Ok((cert_pem, key_pem)) => {
                    if let Err(e) = save_cert_files(&cert_path, &key_path, &cert_pem, &key_pem) {
                        error!("ACME 证书保存失败 ({}): {}", site.name, e);
                    } else {
                        info!("ACME SAN 证书申请成功: {} ({})", site.name, domains.join(", "));
                        reload_acme_cert_in_resolvers(
                            &cert_path, &key_path,
                            &site.server_name,
                            &sni_resolvers,
                        );
                    }
                }
                Err(e) => {
                    error!("ACME 证书申请失败 ({}): {}", site.name, e);
                    // 指数退避重试：1min → 5min → 30min → 2h
                    let backoff_steps: &[u64] = &[60, 300, 1800, 7200];
                    let mut last_err = e;
                    let mut succeeded = false;
                    for &wait_secs in backoff_steps {
                        warn!("ACME 将在 {}s 后重试: {}", wait_secs, site.name);
                        tokio::time::sleep(Duration::from_secs(wait_secs)).await;
                        let retry_result = if use_dns01 {
                            match &tls.dns_provider {
                                Some(provider) => request_acme_cert_dns01(&domains, &email, &tls.acme_provider, provider).await,
                                None => break,
                            }
                        } else {
                            request_acme_cert(&domains, &email, &tls.acme_provider).await
                        };
                        match retry_result {
                            Ok((cert_pem, key_pem)) => {
                                if let Err(e) = save_cert_files(&cert_path, &key_path, &cert_pem, &key_pem) {
                                    error!("ACME 证书保存失败 ({}): {}", site.name, e);
                                } else {
                                    info!("ACME 证书重试成功: {}", site.name);
                                    reload_acme_cert_in_resolvers(&cert_path, &key_path, &site.server_name, &sni_resolvers);
                                }
                                succeeded = true;
                                break;
                            }
                            Err(e) => { last_err = e; }
                        }
                    }
                    if !succeeded {
                        error!("ACME 多次重试均失败 ({})，等待12h后再次尝试: {}", site.name, last_err);
                    }
                }
            }
        }

        // 每 12 小时检查一次
        tokio::time::sleep(Duration::from_secs(12 * 3600)).await;
    }
}

// ─────────────────────────────────────────────
// 内部实现：证书申请
// ─────────────────────────────────────────────

/// 通过 instant-acme（HTTP-01）申请 SAN 多域名证书
///
/// HTTP-01：Let's Encrypt 访问 http://domain/.well-known/acme-challenge/<token>
/// Sweety 在 80 端口响应，完全不依赖 443 是否已有证书。
/// 一次申请覆盖 `domains` 中所有域名（SAN 证书）。
///
/// `acme_provider` 支持: letsencrypt / letsencrypt_staging / zerossl / buypass / 自定义 URL
async fn request_acme_cert(domains: &[String], email: &str, acme_provider: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    use instant_acme::{Account, AccountCredentials, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus};
    use rcgen::{CertificateParams, DistinguishedName, KeyPair};

    if domains.is_empty() { bail!("域名列表为空"); }
    let domains_display = domains.join(", ");

    // 根据 provider 选择 ACME 目录 URL
    let directory_url = match acme_provider {
        "letsencrypt"         => LETS_ENCRYPT_PROD,
        "letsencrypt_staging" => LETS_ENCRYPT_STAGING,
        "zerossl"             => ZEROSSL,
        "buypass"             => BUYPASS,
        "litessl"             => LITESSL,
        custom                => custom,
    };
    info!("ACME HTTP-01 使用提供商: {} ({})，域名: {}", acme_provider, directory_url, domains_display);

    let cache_dir = acme_cache_dir();
    std::fs::create_dir_all(&cache_dir)
        .with_context(|| format!("创建 ACME 缓存目录失败: {}", cache_dir.display()))?;

    // 尝试加载缓存的账号凭据，否则新建账号
    // 按 provider + email 区分缓存，切换 provider 后不会误用旧账号
    let provider_key = acme_provider.replace('/', "_").replace(':', "_");
    let creds_path = cache_dir.join(format!("{}_{}.json", provider_key, email.replace('@', "_").replace('.', "_")));
    let account = if creds_path.exists() {
        let json = std::fs::read_to_string(&creds_path)
            .with_context(|| format!("读取 ACME 账号缓存失败: {}", creds_path.display()))?;
        let creds: AccountCredentials = serde_json::from_str(&json)
            .context("ACME 账号缓存格式无效")?;
        Account::from_credentials(creds).await
            .context("从缓存恢复 ACME 账号失败")?
    } else {
        let (account, creds) = Account::create(
            &NewAccount {
                contact: &[&format!("mailto:{}", email)],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            directory_url,
            None,
        ).await.context("创建 ACME 账号失败")?;
        let json = serde_json::to_string(&creds).context("序列化 ACME 账号凭据失败")?;
        std::fs::write(&creds_path, json)
            .with_context(|| format!("保存 ACME 账号凭据失败: {}", creds_path.display()))?;
        account
    };

    // 创建新订单（所有域名作为 SAN）
    let identifiers: Vec<Identifier> = domains.iter()
        .map(|d| Identifier::Dns(d.clone()))
        .collect();
    let mut order = account.new_order(&NewOrder {
        identifiers: &identifiers,
    }).await.context("创建 ACME 订单失败")?;

    // 获取所有域名的 HTTP-01 challenge（每个域名一个 authorization）
    let authorizations = order.authorizations().await.context("获取 ACME 授权失败")?;
    let mut challenges_to_cleanup: Vec<String> = Vec::new();

    for auth in &authorizations {
        let auth_domain = match &auth.identifier {
            Identifier::Dns(d) => d.as_str(),
        };
        let challenge = auth.challenges.iter()
            .find(|c| c.r#type == ChallengeType::Http01)
            .with_context(|| format!("域名 {} 没有 HTTP-01 challenge", auth_domain))?;

        let key_auth = order.key_authorization(challenge);
        let token = challenge.token.clone();

        // 写入全局 token map，HTTP handler 会响应 /.well-known/acme-challenge/<token>
        ACME_HTTP01_TOKENS.insert(token.clone(), key_auth.as_str().to_string());
        challenges_to_cleanup.push(token);

        // 通知 ACME 服务器可以开始验证
        order.set_challenge_ready(&challenge.url).await
            .with_context(|| format!("通知 ACME challenge ready 失败 ({})", auth_domain))?;
    }

    // 等待订单完成（最多 5 分钟轮询）
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let status = order.refresh().await.context("刷新 ACME 订单状态失败")?;
        match status.status {
            OrderStatus::Ready => break,
            OrderStatus::Valid => break,
            OrderStatus::Invalid => {
                for t in &challenges_to_cleanup { ACME_HTTP01_TOKENS.remove(t); }
                bail!("ACME 订单验证失败（Invalid），请检查所有域名 DNS 解析和 80 端口是否可达: {}", domains_display);
            }
            OrderStatus::Pending => {
                if std::time::Instant::now() > deadline {
                    for t in &challenges_to_cleanup { ACME_HTTP01_TOKENS.remove(t); }
                    bail!("ACME 订单验证超时（5分钟）: {}", domains_display);
                }
                info!("ACME 等待验证中... ({})", domains_display);
            }
            OrderStatus::Processing => {
                if std::time::Instant::now() > deadline {
                    for t in &challenges_to_cleanup { ACME_HTTP01_TOKENS.remove(t); }
                    bail!("ACME 订单处理超时（5分钟）");
                }
            }
        }
    }

    // 清理 token
    for t in &challenges_to_cleanup { ACME_HTTP01_TOKENS.remove(t); }

    // 生成 CSR 并提交（所有域名作为 SAN）
    let key_pair = KeyPair::generate().context("生成 ACME 密钥对失败")?;
    let mut params = CertificateParams::new(domains.to_vec())
        .context("构建证书参数失败")?;
    params.distinguished_name = DistinguishedName::new();
    let csr = params.serialize_request(&key_pair).context("生成 CSR 失败")?;
    let csr_der = csr.der();

    order.finalize(csr_der).await.context("提交 CSR 失败")?;

    // 等待证书签发
    let cert_chain_pem = loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        match order.certificate().await.context("获取签发证书失败")? {
            Some(pem) => break pem,
            None => {
                if std::time::Instant::now() > deadline {
                    bail!("ACME 证书签发超时");
                }
            }
        }
    };

    // serialize_der() 返回 DER 字节，按 64 字符一行折行后包装成 PEM
    let key_der = key_pair.serialize_der();
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&key_der);
    let key_pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        b64.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect::<Vec<_>>().join("\n")
    );
    Ok((cert_chain_pem.into_bytes(), key_pem.into_bytes()))
}

/// 通过 instant-acme（DNS-01）申请 SAN 多域名证书，支持通配符（*.example.com）
///
/// DNS-01：在 DNS 上设置 `_acme-challenge.<domain>` TXT 记录完成验证
/// 不需要 80 端口可达，适合内网/防火墙场景和通配符证书
/// 一次申请覆盖 `domains` 中所有域名（SAN 证书）
async fn request_acme_cert_dns01(
    domains: &[String],
    email: &str,
    acme_provider: &str,
    dns_provider: &crate::config::model::DnsProviderConfig,
) -> Result<(Vec<u8>, Vec<u8>)> {
    use instant_acme::{Account, AccountCredentials, ChallengeType, Identifier, NewAccount, NewOrder, OrderStatus};
    use rcgen::{CertificateParams, DistinguishedName, KeyPair};

    if domains.is_empty() { bail!("域名列表为空"); }
    let domains_display = domains.join(", ");

    let directory_url = match acme_provider {
        "letsencrypt"         => LETS_ENCRYPT_PROD,
        "letsencrypt_staging" => LETS_ENCRYPT_STAGING,
        "zerossl"             => ZEROSSL,
        "buypass"             => BUYPASS,
        "litessl"             => LITESSL,
        custom                => custom,
    };
    info!("ACME DNS-01 使用提供商: {} ({})，域名: {}", acme_provider, directory_url, domains_display);

    let cache_dir = acme_cache_dir();
    std::fs::create_dir_all(&cache_dir)
        .with_context(|| format!("创建 ACME 缓存目录失败: {}", cache_dir.display()))?;

    // 加载或创建 ACME 账号
    // 按 provider + email 区分缓存，切换 provider 后不会误用旧账号
    let provider_key = acme_provider.replace('/', "_").replace(':', "_");
    let creds_path = cache_dir.join(format!("{}_{}.json", provider_key, email.replace('@', "_").replace('.', "_")));
    let account = if creds_path.exists() {
        let json = std::fs::read_to_string(&creds_path)?;
        let creds: AccountCredentials = serde_json::from_str(&json)?;
        Account::from_credentials(creds).await.context("从缓存恢复 ACME 账号失败")?
    } else {
        let (account, creds) = Account::create(
            &NewAccount {
                contact: &[&format!("mailto:{}", email)],
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            directory_url,
            None,
        ).await.context("创建 ACME 账号失败")?;
        std::fs::write(&creds_path, serde_json::to_string(&creds)?)?;
        account
    };

    // 创建新订单（所有域名作为 SAN）
    let identifiers: Vec<Identifier> = domains.iter()
        .map(|d| Identifier::Dns(d.clone()))
        .collect();
    let mut order = account.new_order(&NewOrder {
        identifiers: &identifiers,
    }).await.context("创建 ACME 订单失败")?;

    let authorizations = order.authorizations().await.context("获取 ACME 授权失败")?;
    let mut cleanup_records: Vec<(String, String)> = Vec::new();

    for auth in &authorizations {
        let auth_domain = match &auth.identifier {
            Identifier::Dns(d) => d.as_str(),
        };
        let challenge = auth.challenges.iter()
            .find(|c| c.r#type == ChallengeType::Dns01)
            .with_context(|| format!("域名 {} 没有 DNS-01 challenge", auth_domain))?;

        let key_auth = order.key_authorization(challenge);
        let txt_value = key_auth.dns_value();

        info!("DNS-01: 设置 TXT 记录 domain={} value={}", auth_domain, txt_value);

        super::dns01::set_dns01_record(dns_provider, auth_domain, &txt_value).await
            .with_context(|| format!("DNS-01 设置 TXT 记录失败 ({})", auth_domain))?;

        cleanup_records.push((auth_domain.to_string(), txt_value.to_string()));

        // 通知 ACME 服务器可以开始验证
        order.set_challenge_ready(&challenge.url).await
            .with_context(|| format!("通知 ACME DNS-01 challenge ready 失败 ({})", auth_domain))?;
    }

    // 所有 challenge 设置完毕后统一等待 DNS 传播
    info!("DNS-01: 等待 DNS 传播（60 秒）... 域名: {}", domains_display);
    tokio::time::sleep(Duration::from_secs(60)).await;

    // 等待订单完成（最多 5 分钟）
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let status = order.refresh().await.context("刷新 ACME 订单状态失败")?;
        match status.status {
            OrderStatus::Ready | OrderStatus::Valid => break,
            OrderStatus::Invalid => {
                for (d, v) in &cleanup_records {
                    super::dns01::delete_dns01_record(dns_provider, d, v).await.ok();
                }
                bail!("ACME DNS-01 订单验证失败（Invalid）: {}", domains_display);
            }
            OrderStatus::Pending | OrderStatus::Processing => {
                if std::time::Instant::now() > deadline {
                    for (d, v) in &cleanup_records {
                        super::dns01::delete_dns01_record(dns_provider, d, v).await.ok();
                    }
                    bail!("ACME DNS-01 订单超时（5 分钟）: {}", domains_display);
                }
                info!("DNS-01: 等待验证中... ({})", domains_display);
            }
        }
    }

    // 清理 DNS TXT 记录
    for (d, v) in &cleanup_records {
        super::dns01::delete_dns01_record(dns_provider, d, v).await
            .unwrap_or_else(|e| warn!("DNS-01 清理 TXT 记录失败 ({}): {}", d, e));
    }

    // 生成 CSR 并提交（所有域名作为 SAN）
    let key_pair = KeyPair::generate().context("生成 ACME 密钥对失败")?;
    let mut params = CertificateParams::new(domains.to_vec())
        .context("构建证书参数失败")?;
    params.distinguished_name = DistinguishedName::new();
    let csr = params.serialize_request(&key_pair).context("生成 CSR 失败")?;

    order.finalize(csr.der()).await.context("提交 CSR 失败")?;

    // 等待证书签发
    let cert_chain_pem = loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        match order.certificate().await.context("获取签发证书失败")? {
            Some(pem) => break pem,
            None => {
                if std::time::Instant::now() > deadline {
                    bail!("ACME DNS-01 证书签发超时");
                }
            }
        }
    };

    use base64::Engine as _;
    let key_der = key_pair.serialize_der();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&key_der);
    let key_pem = format!(
        "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
        b64.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect::<Vec<_>>().join("\n")
    );

    info!("ACME DNS-01 SAN 证书申请成功: {}", domains_display);
    Ok((cert_chain_pem.into_bytes(), key_pem.into_bytes()))
}

// ─────────────────────────────────────────────
// 内部实现：证书管理辅助
// ─────────────────────────────────────────────

/// 检查证书是否需要续期
///
/// 解析 X.509 证书的真实到期日，距到期 < `renew_days_before` 天则返回 true
pub(super) fn cert_needs_renewal(cert_path: &Path, renew_days_before: u64) -> bool {
    let Ok(bytes) = std::fs::read(cert_path) else { return true };

    // 提取第一个 PEM 证书的 DER 字节
    let Ok(Some(der)) = rustls_pemfile::certs(&mut bytes.as_slice()).next().transpose() else {
        return true;
    };

    // 用 x509-parser 解析 DER，获取 not_after 到期时间
    use x509_parser::prelude::*;
    let Ok((_, cert)) = X509Certificate::from_der(der.as_ref()) else {
        return true;
    };

    // not_after 是 ASN.1 GeneralizedTime，转成 Unix 时间戳
    let not_after_ts = cert.validity().not_after.timestamp();
    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days_left = (not_after_ts - now_ts) / 86400;

    info!("证书 {} 还有 {} 天到期（续期阈值: {} 天）",
        cert_path.display(), days_left, renew_days_before);

    days_left < renew_days_before as i64
}

/// ACME 续期成功后将新证书热重载到所有 SniResolver，不重启服务器
pub(super) fn reload_acme_cert_in_resolvers(
    cert_path: &Path,
    key_path: &Path,
    server_names: &[String],
    resolvers: &HashMap<u16, Arc<SniResolver>>,
) {
    match crate::server::tls::load_certified_key_from_path(cert_path, key_path) {
        Ok(ck) => {
            let keys = vec![ck];
            for resolver in resolvers.values() {
                resolver.upsert_site(server_names, keys.clone());
            }
            info!("ACME 证书已热重载到 {} 个 TLS 端口", resolvers.len());
        }
        Err(e) => error!("ACME 证书热重载失败: {}", e),
    }
}

/// 立即为指定站点（或所有 ACME 站点）触发证书续期
///
/// - `site_filter`：Some("site-name") 仅续期指定站点，None 续期所有 ACME 站点
/// - 按站点申请 SAN 多域名证书（一张证书覆盖站点所有域名）
/// - 申请失败时保留当前证书，仅记录错误日志
/// - 返回 (触发数, 跳过数, 错误列表)
pub async fn acme_renew_now(
    cfg: &AppConfig,
    sni_resolvers: &HashMap<u16, Arc<SniResolver>>,
    site_filter: Option<&str>,
) -> (usize, usize, Vec<String>) {
    let mut triggered = 0usize;
    let mut skipped = 0usize;
    let mut errors: Vec<String> = Vec::new();

    for site in &cfg.sites {
        // 按站点名过滤
        if let Some(name) = site_filter {
            if site.name != name { continue; }
        }

        let Some(tls) = &site.tls else { skipped += 1; continue };
        if !tls.acme { skipped += 1; continue }

        let email = match &tls.acme_email {
            Some(e) => e.clone(),
            None => {
                let msg = format!("站点 '{}' 未配置 acme_email，跳过", site.name);
                warn!("{}", msg);
                errors.push(msg);
                continue;
            }
        };

        let use_dns01 = tls.acme_challenge.as_str() == "dns01";

        // 过滤可用域名（HTTP-01 不支持通配符）
        let domains: Vec<String> = site.server_name.iter()
            .filter(|d| {
                if !use_dns01 && d.starts_with("*.") {
                    let msg = format!("HTTP-01 不支持通配符 '{}'，跳过", d);
                    warn!("{}", msg);
                    false
                } else {
                    true
                }
            })
            .cloned()
            .collect();
        if domains.is_empty() {
            errors.push(format!("站点 '{}' 过滤后无可用域名", site.name));
            continue;
        }

        // 用主域名定位证书文件
        let primary = primary_domain(&domains);
        let cert_path = acme_cert_path(primary);
        let key_path  = acme_key_path(primary);

        info!("API 触发即时续期: 站点 '{}' ({} 个域名: {}，{})",
            site.name, domains.len(), domains.join(", "),
            if use_dns01 { "DNS-01" } else { "HTTP-01" });

        let result = if use_dns01 {
            match &tls.dns_provider {
                Some(provider) => {
                    request_acme_cert_dns01(&domains, &email, &tls.acme_provider, provider).await
                }
                None => {
                    Err(anyhow::anyhow!("站点 '{}' 配置了 dns01 但没有 dns_provider", site.name))
                }
            }
        } else {
            request_acme_cert(&domains, &email, &tls.acme_provider).await
        };

        match result {
            Ok((cert_pem, key_pem)) => {
                if let Err(e) = save_cert_files(&cert_path, &key_path, &cert_pem, &key_pem) {
                    let msg = format!("证书保存失败 ({}): {}", site.name, e);
                    error!("{}", msg);
                    errors.push(msg);
                } else {
                    info!("API 即时续期成功: {} ({})", site.name, domains.join(", "));
                    reload_acme_cert_in_resolvers(
                        &cert_path, &key_path,
                        &site.server_name,
                        sni_resolvers,
                    );
                    triggered += 1;
                }
            }
            Err(e) => {
                let msg = format!("站点 '{}' 证书申请失败: {}（继续使用当前证书）", site.name, e);
                error!("{}", msg);
                errors.push(msg);
            }
        }
    }

    (triggered, skipped, errors)
}

/// 保存证书文件到磁盘
pub(super) fn save_cert_files(
    cert_path: &Path,
    key_path: &Path,
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<()> {
    if let Some(parent) = cert_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(cert_path, cert_pem)?;
    std::fs::write(key_path, key_pem)?;
    Ok(())
}
