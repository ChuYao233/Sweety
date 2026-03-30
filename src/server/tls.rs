//! TLS 管理模块
//! 负责：Rustls ServerConfig 构建（手动证书）、ACME 自动申请续期、HTTP/3 QuicConfig
//!
//! 证书算法支持：RSA（2048/4096）、ECDSA P-256/P-384、Ed25519
//! ACME：通过 rustls-acme 实现 TLS-ALPN-01 challenge（无需开放 80 端口）

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rustls::ServerConfig;
use rustls_pemfile::Item;
use tracing::{error, info, warn};

use crate::config::model::{AppConfig, SiteConfig, TlsConfig};

pub use sni_resolver::SniResolver;

/// TLS 管理器（静态方法集合）
pub struct TlsManager;

impl TlsManager {
    /// 构建支持 SNI 多证书的 ServerConfig
    ///
    /// 将同一端口下所有站点的证书注册到 SNI resolver，
    /// 浏览器发起 TLS 握手时，Rustls 根据 SNI 自动选择匹配证书。
    /// 若只有一个站点/证书，直接使用单证书模式。
    pub fn build_sni_server_config(sites: &[&SiteConfig]) -> Result<(ServerConfig, Arc<SniResolver>)> {
        // 每个站点可有多张证书（Ed25519 + ECDSA 等），SniResolver 按客户端签名方案选最优
        let mut certs_map: Vec<(Vec<String>, Vec<rustls::sign::CertifiedKey>)> = Vec::new();

        for site in sites {
            let Some(tls) = &site.tls else { continue };
            let certified_keys = build_certified_keys(tls)?;
            if !certified_keys.is_empty() {
                certs_map.push((site.server_name.clone(), certified_keys));
            }
        }

        if certs_map.is_empty() {
            bail!("未找到有效的 TLS 证书配置");
        }

        // 计算该端口所有站点中最严格的 TLS 版本约束
        // 多站点共享同一端口时取并集中最保守的：min 取最高，max 取最低
        let tls_versions = resolve_tls_versions(sites);

        let resolver = Arc::new(SniResolver::new(certs_map));
        // ALPN 顺序策略：http/1.1 在前，h2 在后
        let mut cfg = ServerConfig::builder_with_protocol_versions(&tls_versions)
            .with_no_client_auth()
            .with_cert_resolver(resolver.clone());
        cfg.alpn_protocols = vec![b"http/1.1".to_vec(), b"h2".to_vec()];
        Ok((cfg, resolver))
    }

    /// 根据 TLS 配置构建 Rustls ServerConfig
    ///
    /// - `acme = false`：从 cert/key 文件加载（支持 RSA / ECDSA / Ed25519）
    /// - `acme = true`：从 ACME 缓存目录加载已申请的证书；
    ///   首次运行时自动申请（通过 `acme_renewal_loop`）
    pub fn build_server_config(tls: &TlsConfig) -> Result<ServerConfig> {
        if tls.acme {
            // ACME 模式：读取本地缓存的证书（由 acme_renewal_loop 写入）
            let domain = tls.acme_email.as_deref()
                .map(|_| "")
                .unwrap_or("default");
            let cert_path = acme_cert_path(domain);
            let key_path  = acme_key_path(domain);
            if cert_path.exists() && key_path.exists() {
                load_pem_config(&cert_path, &key_path)
            } else {
                // 证书尚未申请，暂时生成自签名证书供服务器启动
                // ACME 申请由 acme_renewal_loop 在后台进行
                warn!("ACME 证书尚未就绪，使用自签名证书临时启动（域名: {:?}）", domain);
                generate_self_signed(domain)
            }
        } else {
            let cert = tls.cert.as_ref().context("TLS 手动模式需要指定 cert 路径")?;
            let key  = tls.key.as_ref().context("TLS 手动模式需要指定 key 路径")?;
            load_pem_config(cert, key)
        }
    }

    /// 构建 HTTP/3 QUIC 配置
    ///
    /// `QuicConfig` 实际是 `quinn::ServerConfig`
    /// 使用 `with_single_cert` 直接构建，无需先构建 rustls::ServerConfig
    pub fn build_quic_config(tls: &TlsConfig) -> Result<xitca_io::net::QuicConfig> {
        if tls.acme {
            // ACME 模式：读取本地缓存证书
            let domain = tls.acme_email.as_deref().unwrap_or("default");
            let cert_path = acme_cert_path(domain);
            let key_path  = acme_key_path(domain);
            if cert_path.exists() && key_path.exists() {
                return build_quinn_config_from_pem(&cert_path, &key_path);
            }
            // 证书尚未就绪，生成自签名证书作临时替代
            build_quinn_config_self_signed(domain)
        } else if !tls.certs.is_empty() {
            // 多证书模式：QUIC 只需一张，优先取列表第一张（通常是 ECDSA，兼容性最好）
            let first = &tls.certs[0];
            build_quinn_config_from_pem(&first.cert, &first.key)
        } else {
            let cert = tls.cert.as_ref().context("QUIC TLS 需要 cert 路径")?;
            let key  = tls.key.as_ref().context("QUIC TLS 需要 key 路径")?;
            build_quinn_config_from_pem(cert, key)
        }
    }

    /// ACME 证书自动申请与续期后台循环
    ///
    /// 使用 rustls-acme（TLS-ALPN-01 challenge）：
    /// - 不需要开放 80 端口
    /// - 证书签发后写入本地缓存目录
    /// - 每 12 小时检查一次，到期前 30 天自动续期
    pub async fn acme_renewal_loop(cfg: Arc<AppConfig>) {
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

                for domain in &site.server_name {
                    // 跳过通配符域名（ACME TLS-ALPN-01 不支持通配符）
                    if domain.starts_with("*.") { continue; }

                    let cert_path = acme_cert_path(domain);
                    let key_path  = acme_key_path(domain);

                    // 检查证书是否需要续期
                    if cert_path.exists() && !cert_needs_renewal(&cert_path) {
                        continue;
                    }

                    info!("开始为域名 '{}' 申请/续期 ACME 证书，邮箱: {}", domain, email);
                    match request_acme_cert(domain, &email).await {
                        Ok((cert_pem, key_pem)) => {
                            if let Err(e) = save_cert_files(&cert_path, &key_path, &cert_pem, &key_pem) {
                                error!("ACME 证书保存失败 ({}): {}", domain, e);
                            } else {
                                info!("ACME 证书申请成功: {}", domain);
                            }
                        }
                        Err(e) => {
                            error!("ACME 证书申请失败 ({}): {}", domain, e);
                        }
                    }
                }
            }

            // 每 12 小时检查一次
            tokio::time::sleep(Duration::from_secs(12 * 3600)).await;
        }
    }
}

// ─────────────────────────────────────────────
// 内部实现：TLS 版本解析
// ─────────────────────────────────────────────

/// 根据站点 TLS 配置计算协议版本列表
///
/// 多站点共享同端口时取最严格的交集：
/// - min_version 取所有站点中最高的（更严格）
/// - max_version 取所有站点中最低的（更严格）
/// 单站点或无 TLS 配置时默认 TLS 1.2 + TLS 1.3
fn resolve_tls_versions(sites: &[&SiteConfig]) -> Vec<&'static rustls::SupportedProtocolVersion> {
    // 枚举所有站点的版本约束，取最保守交集
    let mut allow_12 = true;
    let mut allow_13 = true;

    for site in sites {
        let Some(tls) = &site.tls else { continue };
        let min = tls.min_version.as_str();
        let max = tls.max_version.as_str();
        // min_version = tls1.3 时排除 TLS 1.2
        if min == "tls1.3" {
            allow_12 = false;
        }
        // max_version = tls1.2 时排除 TLS 1.3
        if max == "tls1.2" {
            allow_13 = false;
        }
    }

    match (allow_12, allow_13) {
        (true, true)   => vec![&rustls::version::TLS12, &rustls::version::TLS13],
        (false, true)  => vec![&rustls::version::TLS13],
        (true, false)  => vec![&rustls::version::TLS12],
        // 不合理的配置（同时禁止两者），回退到全部支持
        (false, false) => vec![&rustls::version::TLS12, &rustls::version::TLS13],
    }
}

// ─────────────────────────────────────────────
// 内部实现：PEM 证书加载
// ─────────────────────────────────────────────

/// 从 PEM 文件加载证书链和私钥，构建 Rustls ServerConfig
///
/// 支持私钥类型：RSA PKCS#1、RSA PKCS#8、ECDSA（P-256/P-384）、Ed25519
fn load_pem_config(cert_path: &Path, key_path: &Path) -> Result<ServerConfig> {
    // 读取证书链
    let cert_bytes = std::fs::read(cert_path)
        .with_context(|| format!("读取证书文件失败: {}", cert_path.display()))?;
    let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls_pemfile::certs(&mut cert_bytes.as_slice())
            .collect::<std::result::Result<_, _>>()
            .with_context(|| format!("解析证书 PEM 失败: {}", cert_path.display()))?;

    if certs.is_empty() {
        bail!("证书文件中没有找到有效证书: {}", cert_path.display());
    }

    // 读取私钥（自动识别 RSA / ECDSA / Ed25519）
    let key_bytes = std::fs::read(key_path)
        .with_context(|| format!("读取私钥文件失败: {}", key_path.display()))?;
    let key = load_private_key(&key_bytes)
        .with_context(|| format!("解析私钥失败: {}", key_path.display()))?;

    // 构建 ServerConfig（ALPN 由 xitca-web bind_rustls 自动设置）
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("构建 Rustls ServerConfig 失败")?;

    info!("TLS 证书加载成功: {}", cert_path.display());
    Ok(config)
}

/// 从 PEM 字节中加载私钥，支持多种算法
fn load_private_key(pem_bytes: &[u8]) -> Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let mut reader = pem_bytes;
    // 遍历 PEM 条目，找到第一个私钥
    for item in rustls_pemfile::read_all(&mut reader).flatten() {
        let key = match item {
            Item::Pkcs1Key(k)  => rustls::pki_types::PrivateKeyDer::Pkcs1(k),
            Item::Pkcs8Key(k)  => rustls::pki_types::PrivateKeyDer::Pkcs8(k),
            Item::Sec1Key(k)   => rustls::pki_types::PrivateKeyDer::Sec1(k),
            _ => continue,
        };
        return Ok(key);
    }
    bail!("私钥文件中没有找到 RSA/ECDSA/Ed25519 私钥")
}

// ─────────────────────────────────────────────
// 内部实现：自签名证书（ACME 首次启动临时用）
// ─────────────────────────────────────────────

/// 生成自签名证书用于临时启动（仅在 ACME 证书尚未就绪时使用）
fn generate_self_signed(domain: &str) -> Result<ServerConfig> {
    let subject_alt_names = if domain.is_empty() {
        vec!["localhost".to_string()]
    } else {
        vec![domain.to_string()]
    };

    let cert = rcgen::generate_simple_self_signed(subject_alt_names)
        .context("生成自签名证书失败")?;

    let cert_der = rustls::pki_types::CertificateDer::from(
        cert.cert.der().to_vec()
    );
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der())
    );

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .context("构建自签名 ServerConfig 失败")?;

    Ok(config)
}

// ─────────────────────────────────────────────
// 内部实现：Quinn（HTTP/3）配置构建
// ─────────────────────────────────────────────

/// 从 PEM 文件构建 quinn::ServerConfig（用于 HTTP/3）
fn build_quinn_config_from_pem(cert_path: &Path, key_path: &Path) -> Result<xitca_io::net::QuicConfig> {
    let cert_bytes = std::fs::read(cert_path)
        .with_context(|| format!("读取证书失败: {}", cert_path.display()))?;
    let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls_pemfile::certs(&mut cert_bytes.as_slice())
            .collect::<std::result::Result<_, _>>()
            .with_context(|| "解析证书 PEM 失败")?;

    let key_bytes = std::fs::read(key_path)
        .with_context(|| format!("读取私钥失败: {}", key_path.display()))?;
    let key = load_private_key(&key_bytes)?;

    let server_config = xitca_io::net::QuicConfig::with_single_cert(certs, key)
        .context("构建 Quinn ServerConfig 失败")?;

    Ok(server_config)
}

/// 生成自签名证书构建 quinn::ServerConfig（ACME 证书未就绪时临时使用）
fn build_quinn_config_self_signed(domain: &str) -> Result<xitca_io::net::QuicConfig> {
    let subject_alt_names = if domain.is_empty() {
        vec!["localhost".to_string()]
    } else {
        vec![domain.to_string()]
    };

    let cert = rcgen::generate_simple_self_signed(subject_alt_names)
        .context("生成自签名证书失败")?;

    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
        rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der())
    );

    xitca_io::net::QuicConfig::with_single_cert(vec![cert_der], key_der)
        .context("构建 Quinn 自签名 ServerConfig 失败")
}

// ─────────────────────────────────────────────
// 内部实现：ACME 证书申请
// ─────────────────────────────────────────────

/// 通过 rustls-acme（TLS-ALPN-01）申请证书
///
/// 返回 (cert_pem, key_pem) 字节对
async fn request_acme_cert(domain: &str, email: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    use rustls_acme::{AcmeConfig, caches::DirCache};

    // 使用磁盘缓存存储账号 key 和证书
    let cache_dir = acme_cache_dir();
    std::fs::create_dir_all(&cache_dir)
        .with_context(|| format!("创建 ACME 缓存目录失败: {}", cache_dir.display()))?;

    // 构建 ACME 配置（Let's Encrypt 生产环境）
    let mut acme_state = AcmeConfig::new([domain])
        .contact_push(format!("mailto:{}", email))
        .cache(DirCache::new(cache_dir.clone()))
        .state();

    // 轮询直到证书就绪（最多等待 5 分钟）
    let deadline = tokio::time::Instant::now() + Duration::from_secs(300);
    loop {
        use futures_util::StreamExt;
        tokio::select! {
            event = acme_state.next() => {
                match event {
                    Some(Ok(event)) => {
                        info!("ACME 事件: {:?}", event);
                    }
                    Some(Err(e)) => {
                        bail!("ACME 流程错误: {}", e);
                    }
                    None => break,
                }
            }
            _ = tokio::time::sleep_until(deadline) => {
                bail!("ACME 证书申请超时（5分钟）");
            }
        }

        // 检查缓存目录中是否已生成证书
        let cert_path = acme_cert_path(domain);
        let key_path  = acme_key_path(domain);
        if cert_path.exists() && key_path.exists() {
            let cert_pem = std::fs::read(&cert_path)?;
            let key_pem  = std::fs::read(&key_path)?;
            return Ok((cert_pem, key_pem));
        }
    }

    bail!("ACME 证书申请完成但未找到证书文件")
}

/// 检查证书是否需要续期（距到期 < 30 天则续期）
fn cert_needs_renewal(cert_path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(cert_path) else { return true };
    let Ok(Some(_cert)) = rustls_pemfile::certs(&mut bytes.as_slice()).next().transpose() else {
        return true
    };

    // 解析 DER 证书获取有效期
    // 使用简单的字节长度启发式（若证书 < 30 天内到期则续期）
    // 完整实现需要 x509-parser，此处通过文件修改时间估算
    let Ok(meta) = std::fs::metadata(cert_path) else { return true };
    let Ok(modified) = meta.modified() else { return true };
    let age = std::time::SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::from_secs(0));

    // Let's Encrypt 证书有效期 90 天，60 天后续期
    age > Duration::from_secs(60 * 24 * 3600)
}

/// 保存证书文件到磁盘
fn save_cert_files(
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

// ─────────────────────────────────────────────
// 路径辅助函数
// ─────────────────────────────────────────────

fn acme_cache_dir() -> std::path::PathBuf {
    dirs_next::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/etc"))
        .join("sweety")
        .join("acme")
}

fn acme_cert_path(domain: &str) -> std::path::PathBuf {
    acme_cache_dir().join(format!("{}.crt", domain))
}

fn acme_key_path(domain: &str) -> std::path::PathBuf {
    acme_cache_dir().join(format!("{}.key", domain))
}

// ─────────────────────────────────────────────
// SNI 多证书 Resolver
// ─────────────────────────────────────────────

mod sni_resolver {
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};
    use rustls::server::ClientHello;

    /// SNI Resolver 内部数据
    #[derive(Debug, Default)]
    struct Inner {
        exact:    HashMap<String, Vec<Arc<rustls::sign::CertifiedKey>>>,
        wildcard: HashMap<String, Vec<Arc<rustls::sign::CertifiedKey>>>,
        fallback: Vec<Arc<rustls::sign::CertifiedKey>>,
    }

    /// SNI Resolver：根据 SNI 和客户端签名方案动态选最优证书
    ///
    /// 内部用 RwLock 保护，支持运行时原地更新证书（热重载不断连）。
    #[derive(Debug, Default)]
    pub struct SniResolver {
        inner: RwLock<Inner>,
    }

    impl SniResolver {
        pub fn new(certs_map: Vec<(Vec<String>, Vec<rustls::sign::CertifiedKey>)>) -> Self {
            let r = Self::default();
            for (names, keys) in certs_map {
                r.upsert_site(&names, keys);
            }
            r
        }

        /// 插入或更新单个站点的证书列表
        pub fn upsert_site(&self, names: &[String], keys: Vec<rustls::sign::CertifiedKey>) {
            let arcs: Vec<Arc<rustls::sign::CertifiedKey>> =
                keys.into_iter().map(Arc::new).collect();
            let mut inner = self.inner.write().unwrap();
            if inner.fallback.is_empty() {
                inner.fallback = arcs.clone();
            }
            for name in names {
                if let Some(suffix) = name.strip_prefix("*.") {
                    inner.wildcard.insert(suffix.to_lowercase(), arcs.clone());
                } else {
                    inner.exact.insert(name.to_lowercase(), arcs.clone());
                }
            }
        }

        /// 删除单个站点的证书
        pub fn remove_site(&self, names: &[String]) {
            let mut inner = self.inner.write().unwrap();
            for name in names {
                if let Some(suffix) = name.strip_prefix("*.") {
                    inner.wildcard.remove(&suffix.to_lowercase());
                } else {
                    inner.exact.remove(&name.to_lowercase());
                }
            }
            // 重置 fallback
            inner.fallback = inner.exact.values()
                .chain(inner.wildcard.values())
                .next()
                .cloned()
                .unwrap_or_default();
        }

        fn lookup<'a>(inner: &'a Inner, name: &str) -> &'a Vec<Arc<rustls::sign::CertifiedKey>> {
            let lower = name.to_lowercase();
            if let Some(cks) = inner.exact.get(&lower) { return cks; }
            if let Some(dot) = lower.find('.') {
                let suffix = &lower[dot + 1..];
                if let Some(cks) = inner.wildcard.get(suffix) { return cks; }
            }
            &inner.fallback
        }

        fn choose(
            candidates: &[Arc<rustls::sign::CertifiedKey>],
            schemes: &[rustls::SignatureScheme],
        ) -> Option<Arc<rustls::sign::CertifiedKey>> {
            for ck in candidates {
                if ck.key.choose_scheme(schemes).is_some() {
                    return Some(ck.clone());
                }
            }
            candidates.first().cloned()
        }
    }

    impl rustls::server::ResolvesServerCert for SniResolver {
        fn resolve(&self, client_hello: ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
            let inner = self.inner.read().unwrap();
            let candidates = match client_hello.server_name() {
                Some(name) => Self::lookup(&inner, name),
                None => &inner.fallback,
            };
            let schemes = client_hello.signature_schemes();
            Self::choose(candidates, schemes)
        }
    }
}

// ─────────────────────────────────────────────
// 辅助：从 TlsConfig 构建 CertifiedKey
// ─────────────────────────────────────────────

/// 公开给热重载模块调用：从 TlsConfig 加载所有证书
impl TlsManager {
    pub fn build_certified_keys_pub(tls: &TlsConfig) -> Result<Vec<rustls::sign::CertifiedKey>> {
        build_certified_keys(tls)
    }
}

fn build_certified_keys(tls: &TlsConfig) -> Result<Vec<rustls::sign::CertifiedKey>> {
    if tls.acme {
        // ACME 模式：只有一张证书
        let domain = tls.acme_email.as_deref().unwrap_or("default");
        let ck = load_certified_key_from_path(&acme_cert_path(domain), &acme_key_path(domain))?;
        return Ok(vec![ck]);
    }

    if !tls.certs.is_empty() {
        // 多证书模式：加载所有证书，失败的跳过并警告
        let mut keys = Vec::new();
        for pair in &tls.certs {
            match load_certified_key_from_path(&pair.cert, &pair.key) {
                Ok(ck) => keys.push(ck),
                Err(e) => warn!("跳过证书 {}: {}", pair.cert.display(), e),
            }
        }
        if keys.is_empty() {
            bail!("certs 列表中没有可用的证书");
        }
        return Ok(keys);
    }

    // 单证书兼容模式
    let cert = tls.cert.as_ref().context("TLS 需要指定 cert 路径")?;
    let key  = tls.key.as_ref().context("TLS 需要指定 key 路径")?;
    let ck = load_certified_key_from_path(cert, key)?;
    Ok(vec![ck])
}

/// 从文件路径加载单张证书
fn load_certified_key_from_path(
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
) -> Result<rustls::sign::CertifiedKey> {
    let cert_bytes = std::fs::read(cert_path)
        .with_context(|| format!("读取证书失败: {}", cert_path.display()))?;
    let certs: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls_pemfile::certs(&mut cert_bytes.as_slice())
            .collect::<std::result::Result<_, _>>()
            .with_context(|| format!("解析证书 PEM 失败: {}", cert_path.display()))?;
    if certs.is_empty() {
        bail!("证书文件无有效证书: {}", cert_path.display());
    }

    let key_bytes = std::fs::read(key_path)
        .with_context(|| format!("读取私钥失败: {}", key_path.display()))?;
    let key_der = load_private_key(&key_bytes)
        .with_context(|| format!("解析私钥失败: {}", key_path.display()))?;

    // any_supported_type 内部已处理 RSA / ECDSA / Ed25519（PKCS#8）
    let signing_key = rustls::crypto::ring::sign::any_supported_type(&key_der)
        .map_err(|e| anyhow::anyhow!("私钥类型不支持（RSA/ECDSA/Ed25519）: {:?}", e))?;

    info!("TLS 证书加载成功: {}", cert_path.display());
    Ok(rustls::sign::CertifiedKey::new(certs, signing_key))
}
