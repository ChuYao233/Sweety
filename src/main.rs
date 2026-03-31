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
/// 快速开始：
///   sweety start             # 后台启动（daemon）
///   sweety run               # 前台启动
///   sweety stop              # 停止后台进程
///   sweety restart           # 重启
///   sweety reload            # 热重载配置（不断连）
///   sweety validate          # 验证配置 + 证书
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

    /// 配置文件路径
    #[arg(short = 'c', long, default_value = "config/sweety.toml", global = true)]
    config: PathBuf,

    /// 输出版本信息（-v / --ver）
    #[arg(short = 'v', long = "ver", action = clap::ArgAction::Version, global = true)]
    _version: (),

    /// PID 文件路径（daemon 模式用）
    #[arg(long, default_value = "/var/run/sweety.pid", global = true)]
    pid_file: PathBuf,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// 前台运行（不进入后台，推荐 systemd / supervisord 下使用）
    Run,

    /// 后台启动（daemon，写 PID 文件，对标 Caddy start）
    ///
    /// 示例：
    ///   sweety start
    ///   sweety start --config /etc/sweety/sweety.toml
    ///   sweety start --pid-file /var/run/sweety.pid
    Start,

    /// 停止后台运行的 Sweety（读取 PID 文件发送 SIGTERM）
    Stop,

    /// 重启：stop + start
    Restart,

    /// 热重载配置（不断开现有连接）——需要 global.admin_listen 已配置
    Reload,

    /// 验证配置文件语法和 TLS 证书（等价 nginx -t）
    Validate,

    /// 输出 Admin REST API 文档 JSON
    #[command(name = "api-doc")]
    ApiDoc,

    /// 输出版本信息
    Version,
}

fn main() {
    let cli = Cli::parse();
    let config = &cli.config;
    let pid_file = &cli.pid_file;

    match cli.command.unwrap_or(Commands::Run) {
        Commands::ApiDoc   => cmd_api_doc(),
        Commands::Version  => cmd_version(),
        Commands::Validate => cmd_validate(config),
        Commands::Reload   => cmd_reload(config),
        Commands::Run      => cmd_run(config),
        Commands::Start    => cmd_start(config, pid_file),
        Commands::Stop     => cmd_stop(pid_file),
        Commands::Restart  => { cmd_stop(pid_file); cmd_start(config, pid_file); },
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

    // 安装全局 panic hook：将 panic 信息路由到 tracing error！日志
    // 默认 panic 只输出到 stderr，日志系统感知不到；hook 后可写入日志文件、监控系统
    install_panic_hook();

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

/// 后台启动（daemon 模式）
///
/// Unix: fork 子进程，父进程退出，子进程继续运行并写 PID 文件
/// Windows: 直接 spawn 一个新进程（detached），写 PID 文件
fn cmd_start(config: &PathBuf, pid_file: &PathBuf) {
    // 先验证配置，失败直接退出
    init_stderr_log();
    let _cfg = load_cfg_or_exit(config);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // 用 setsid 创建新会话，脱离终端
        let exe = std::env::current_exe().unwrap_or_else(|_| "sweety".into());
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("run")
           .arg("--config").arg(config)
           .arg("--pid-file").arg(pid_file);
        // 关闭标准输入，重定向输出到 /dev/null
        cmd.stdin(std::process::Stdio::null())
           .stdout(std::process::Stdio::null())
           .stderr(std::process::Stdio::null());
        unsafe { cmd.pre_exec(|| { libc::setsid(); Ok(()) }); }
        match cmd.spawn() {
            Ok(child) => {
                let pid = child.id();
                if let Some(parent) = pid_file.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(e) = std::fs::write(pid_file, pid.to_string()) {
                    eprintln!("[WARN] 写 PID 文件失败 {}: {}", pid_file.display(), e);
                }
                println!("Sweety started (PID {})", pid);
                println!("PID file: {}", pid_file.display());
            }
            Err(e) => {
                eprintln!("[ERROR] 启动失败: {}", e);
                std::process::exit(1);
            }
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        let exe = std::env::current_exe().unwrap_or_else(|_| "sweety.exe".into());
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("run")
           .arg("--config").arg(config)
           .arg("--pid-file").arg(pid_file)
           .stdin(std::process::Stdio::null())
           .stdout(std::process::Stdio::null())
           .stderr(std::process::Stdio::null())
           .creation_flags(DETACHED_PROCESS);
        match cmd.spawn() {
            Ok(child) => {
                let pid = child.id();
                if let Err(e) = std::fs::write(pid_file, pid.to_string()) {
                    eprintln!("[WARN] 写 PID 文件失败: {}", e);
                }
                println!("Sweety started (PID {})", pid);
            }
            Err(e) => {
                eprintln!("[ERROR] 启动失败: {}", e);
                std::process::exit(1);
            }
        }
    }
}

/// 停止后台运行的 Sweety（读取 PID 文件，发送 SIGTERM）
fn cmd_stop(pid_file: &PathBuf) {
    let pid_str = match std::fs::read_to_string(pid_file) {
        Ok(s) => s.trim().to_string(),
        Err(_) => {
            eprintln!("[ERROR] 找不到 PID 文件: {}，Sweety 可能未在运行", pid_file.display());
            std::process::exit(1);
        }
    };
    let pid: u32 = match pid_str.parse() {
        Ok(p) => p,
        Err(_) => {
            eprintln!("[ERROR] PID 文件内容无效: {}", pid_str);
            std::process::exit(1);
        }
    };

    #[cfg(unix)]
    {
        let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
        if ret == 0 {
            let _ = std::fs::remove_file(pid_file);
            println!("Sweety stopped (PID {})", pid);
        } else {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ESRCH) {
                eprintln!("[WARN] 进程 {} 不存在，可能已停止", pid);
                let _ = std::fs::remove_file(pid_file);
            } else {
                eprintln!("[ERROR] 发送 SIGTERM 失败: {}", err);
                std::process::exit(1);
            }
        }
    }

    #[cfg(windows)]
    {
        // Windows 没有 SIGTERM，用 taskkill /F /PID
        let status = std::process::Command::new("taskkill")
            .args(["/F", "/PID", &pid.to_string()])
            .status();
        match status {
            Ok(s) if s.success() => {
                let _ = std::fs::remove_file(pid_file);
                println!("Sweety stopped (PID {})", pid);
            }
            Ok(_) => {
                eprintln!("[WARN] 进程 {} 可能已停止", pid);
                let _ = std::fs::remove_file(pid_file);
            }
            Err(e) => {
                eprintln!("[ERROR] taskkill 失败: {}", e);
                std::process::exit(1);
            }
        }
    }
}

/// 安装全局 panic hook
///
/// 将 panic 信息（包含文件/行号/消息）输出到 tracing error 日志，
/// 同时保留原有的 stderr 输出行为，确保运维人员能通过日志系统感知崩溃
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 提取 panic 消息
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            *s
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.as_str()
        } else {
            "(unknown panic payload)"
        };

        // 提取文件/行号位置
        let location = info.location().map(|l| {
            format!("{}:{}", l.file(), l.line())
        }).unwrap_or_else(|| "(unknown location)".to_string());

        // 输出到 tracing 日志（会写入日志文件 / 监控 / 结构化输出）
        error!(
            panic.message = msg,
            panic.location = %location,
            "PANIC: {} at {}",
            msg, location,
        );

        // 调用默认 hook 保留 stderr 输出（方便直接查看终端）
        default_hook(info);
    }));
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
