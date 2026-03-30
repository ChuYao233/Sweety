//! Sweety Web 服务器 —— 程序入口
//! 负责：CLI 参数解析、日志初始化、配置加载、启动服务器

use clap::Parser;
use std::path::PathBuf;
use tracing::{error, info};

use sweety_lib::{config::loader::load_config, server::http::SweetyServer};

/// Sweety —— 高性能多站点 Web 服务器（HTTP/1.1 + HTTP/2 + HTTP/3 + TLS + WebSocket）
#[derive(Parser, Debug)]
#[command(name = "sweety", version, about)]
struct Cli {
    /// 配置文件路径（支持 .toml / .json / .yaml）
    #[arg(short, long, default_value = "config/sweety.toml")]
    config: PathBuf,

    /// 仅测试配置文件语法，不启动服务器
    #[arg(short, long, default_value_t = false)]
    test: bool,
}

fn main() {
    let cli = Cli::parse();

    // 先加载配置文件（用于获取日志级别）
    let cfg = match load_config(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            // 配置加载失败时用临时 stderr 输出错误，再退出
            eprintln!("[ERROR] 配置文件加载失败: {:#}", e);
            std::process::exit(1);
        }
    };

    // 初始化日志
    // 优先级：RUST_LOG 环境变量 > 配置文件 log_level > 默认 info
    // 支持细粒度过滤，如：sweety=debug,xitca_server=warn
    let log_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cfg.global.log_level));

    tracing_subscriber::fmt()
        .with_env_filter(log_filter)
        .init();

    if cli.test {
        info!("配置文件语法正确，共 {} 个站点", cfg.sites.len());
        return;
    }

    info!(
        "Sweety {} 正在启动，共 {} 个站点（日志级别: {}）",
        env!("CARGO_PKG_VERSION"),
        cfg.sites.len(),
        cfg.global.log_level,
    );

    // 启动服务器（xitca-web 内部管理 tokio 运行时）
    if let Err(e) = SweetyServer::new(cfg).run() {
        error!("服务器启动失败: {:#}", e);
        std::process::exit(1);
    }
}
