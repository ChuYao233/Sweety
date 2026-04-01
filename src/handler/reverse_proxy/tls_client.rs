//! TLS 客户端模块
//! 负责：为上游连接构建 TLS 客户端配置，支持标准证书验证和跳过验证两种模式

use std::sync::Arc;

use anyhow::Result;
use rustls::ClientConfig as RustlsClientConfig;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

/// 全局单例 TLS 客户端配置（启用 session cache，跳过证书验证模式）
static TLS_CLIENT_INSECURE: std::sync::OnceLock<Arc<RustlsClientConfig>> = std::sync::OnceLock::new();
/// 全局单例 TLS 客户端配置（启用 session cache，标准证书验证模式）
static TLS_CLIENT_SECURE:   std::sync::OnceLock<Arc<RustlsClientConfig>> = std::sync::OnceLock::new();

/// 对已建立的 TCP 连接执行 TLS 握手，返回加密流
///
/// ClientConfig 全局单例：
/// - 启用 session cache（ClientSessionMemoryCache 1024），允许 TLS session resumption
/// - 避免每次请求新建 config 导致 session cache 失效
pub async fn tls_connect(
    tcp: TcpStream,
    sni: &str,
    insecure: bool,
) -> Result<tokio_rustls::client::TlsStream<TcpStream>> {
    let config = if insecure {
        TLS_CLIENT_INSECURE.get_or_init(build_insecure_config).clone()
    } else {
        TLS_CLIENT_SECURE.get_or_init(build_secure_config).clone()
    };
    let connector = TlsConnector::from(config);
    let server_name = rustls::pki_types::ServerName::try_from(sni.to_string())
        .map_err(|e| anyhow::anyhow!("无效的 TLS SNI '{}': {}", sni, e))?;
    connector.connect(server_name, tcp).await
        .map_err(|e| anyhow::anyhow!("TLS 握手失败 ({}): {}", sni, e))
}

// ─────────────────────────────────────────────
// 内部实现
// ─────────────────────────────────────────────

/// 跳过证书验证的 TLS 配置（insecure 模式）
fn build_insecure_config() -> Arc<RustlsClientConfig> {
    #[derive(Debug)]
    struct NoVerifier;

    impl rustls::client::danger::ServerCertVerifier for NoVerifier {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls::pki_types::CertificateDer<'_>,
            _intermediates: &[rustls::pki_types::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp_response: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &rustls::pki_types::CertificateDer<'_>,
            _dss: &rustls::DigitallySignedStruct,
        ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::aws_lc_rs::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    let mut cfg = RustlsClientConfig::builder_with_provider(
        Arc::new(rustls::crypto::aws_lc_rs::default_provider())
    )
        .with_safe_default_protocol_versions().unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier))
        .with_no_client_auth();
    // 启用 session cache，允许 TLS session resumption，避免每次全量握手
    cfg.resumption = rustls::client::Resumption::in_memory_sessions(1024);
    Arc::new(cfg)
}

/// 使用 webpki 根证书的标准 TLS 配置
fn build_secure_config() -> Arc<RustlsClientConfig> {
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut cfg = RustlsClientConfig::builder_with_provider(
        Arc::new(rustls::crypto::aws_lc_rs::default_provider())
    )
        .with_safe_default_protocol_versions().unwrap()
        .with_root_certificates(root_store)
        .with_no_client_auth();
    cfg.resumption = rustls::client::Resumption::in_memory_sessions(1024);
    Arc::new(cfg)
}
