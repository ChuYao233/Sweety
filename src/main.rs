//! Sweety Web 服务器 —— 程序入口
//!
//! # 快速开始
//! ```sh
//! sweety run                          # 前台运行（推荐）
//! sweety run --config /etc/sweety.toml
//! sweety validate --config x.toml    # 验证配置
//! sweety reload                       # 热重载配置（不断连）
//! sweety api-doc                      # 输出 Admin API 文档 JSON
//! ```

#[cfg(all(not(windows), feature = "jemalloc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::{error, info};

use sweety_lib::{config::loader::load_config, server::http::SweetyServer};

/// Sweety —— 高性能多站点 Web 服务器
///
/// HTTP/1.1 + HTTP/2 + HTTP/3 + TLS + WebSocket + 反向代理 + 静态文件
///
/// 快速开始：
///   sweety run
///   sweety run --config /etc/sweety.toml
///   sweety validate
///   sweety api-doc
#[derive(Parser, Debug)]
#[command(
    name    = "sweety",
    version,
    about   = "High-performance multi-site web server",
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// 配置文件路径（支持 .toml）
    #[arg(short, long, default_value = "config/sweety.toml", global = true)]
    config: PathBuf,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// 在前台启动 Sweety 并持续运行（推荐用于生产环境）
    ///
    /// 示例：
    ///   sweety run
    ///   sweety run --config /etc/sweety/sweety.toml
    Run,

    /// 验证配置文件语法和 TLS 证书（不启动服务，等价 nginx -t）
    ///
    /// 示例：
    ///   sweety validate
    ///   sweety validate --config /etc/sweety/sweety.toml
    Validate,

    /// 向运行中的 Sweety 发送热重载信号（配置不断连应用）
    ///
    /// 需要 global.admin_listen 已配置。
    ///
    /// 示例：
    ///   sweety reload
    ///   sweety reload --config /etc/sweety/sweety.toml
    Reload,

    /// 输出 Admin REST API 所有接口的 JSON 文档（面板对接用）
    ///
    /// 示例：
    ///   sweety api-doc
    ///   sweety api-doc | jq '.endpoints[] | .path'
    #[command(name = "api-doc")]
    ApiDoc,

    /// 输出当前版本信息
    Version,
}

fn main() {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Commands::Run) {
        Commands::ApiDoc => cmd_api_doc(),
        Commands::Version => cmd_version(),
        Commands::Validate => cmd_validate(&cli.config),
        Commands::Reload => cmd_reload(&cli.config),
        Commands::Run => cmd_run(&cli.config),
    }
}

// ─────────────────────────────────────────────
// 子命令实现
// ─────────────────────────────────────────────

fn cmd_api_doc() {
    let doc = sweety_lib::admin_api::http::build_api_doc();
    println!("{}", serde_json::to_string_pretty(&doc).unwrap_or_default());
}

fn cmd_version() {
    println!(
        "Sweety {}\nBuilt with Rust {}",
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_RUST_VERSION", "(unknown)"),
    );
}

fn cmd_validate(config: &PathBuf) {
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

fn cmd_reload(config: &PathBuf) {
    init_stderr_log();
    let cfg = load_cfg_or_exit(config);
    let addr = &cfg.global.admin_listen;
    if addr.is_empty() {
        eprintln!("[ERROR] global.admin_listen 未配置，无法发送 reload 信号");
        eprintln!("请在配置文件中添加: admin_listen = \"127.0.0.1:9000\"");
        std::process::exit(1);
    }
    // 向 admin API 发送 POST /api/v1/reload
    let url = format!("http://{}/api/v1/reload", addr);
    info!("发送热重载请求到: {}", url);
    let token = &cfg.global.admin_token;
    match ureq_reload(&url, token) {
        Ok(_)  => info!("热重载成功"),
        Err(e) => {
            eprintln!("[ERROR] 热重载失败: {}", e);
            std::process::exit(1);
        }
    }
}

fn cmd_run(config: &PathBuf) {
    let cfg = load_cfg_or_exit(config);
    let log_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cfg.global.log_level));
    tracing_subscriber::fmt().with_env_filter(log_filter).init();

    info!(
        "Sweety {} 正在启动，共 {} 个站点（日志级别: {}）",
        env!("CARGO_PKG_VERSION"),
        cfg.sites.len(),
        cfg.global.log_level,
    );
    if let Err(e) = SweetyServer::new(cfg).with_config_path(config.clone()).run() {
        error!("服务器启动失败: {:#}", e);
        std::process::exit(1);
    }
}

// ─────────────────────────────────────────────
// 辅助函数
// ─────────────────────────────────────────────

fn load_cfg_or_exit(config: &PathBuf) -> sweety_lib::config::model::AppConfig {
    match load_config(config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ERROR] 配置文件加载失败: {:#}", e);
            std::process::exit(1);
        }
    }
}

fn init_stderr_log() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_writer(std::io::stderr)
        .try_init();
}

/// 简单的 HTTP POST（不依赖 reqwest/tokio，用标准库实现）
fn ureq_reload(url: &str, token: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    // 解析 host:port
    let url = url.strip_prefix("http://").unwrap_or(url);
    let (host_port, path) = url.split_once('/').unwrap_or((url, "api/v1/reload"));
    let path = format!("/{}", path);

    let mut stream = TcpStream::connect(host_port)
        .map_err(|e| format!("连接 {} 失败: {}", host_port, e))?;

    let auth = if token.is_empty() {
        String::new()
    } else {
        format!("Authorization: Bearer {}\r\n", token)
    };
    let req = format!(
        "POST {} HTTP/1.0\r\nHost: {}\r\nContent-Length: 0\r\n{}\r\n",
        path, host_port, auth
    );
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;

    let mut resp = String::new();
    stream.read_to_string(&mut resp).map_err(|e| e.to_string())?;
    if resp.starts_with("HTTP/1") && (resp.contains("200") || resp.contains("204")) {
        Ok(())
    } else {
        Err(format!("服务器返回: {}", resp.lines().next().unwrap_or("")))
    }
}
