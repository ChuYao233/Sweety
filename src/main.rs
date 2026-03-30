//! Sweety Web 服务器 —— 程序入口
//! 负责：CLI 参数解析、日志初始化、配置加载、启动服务器

use clap::Parser;
use std::path::PathBuf;
use tracing::{error, info};

use sweety_lib::{config, server};

/// Sweety —— 高性能多站点 Web 服务器
#[derive(Parser, Debug)]
#[command(name = "sweety", version, about)]
struct Cli {
    /// 配置文件路径（支持 .toml / .json / .yaml）
    #[arg(short, long, default_value = "config/sweety.toml")]
    config: PathBuf,

    /// 测试配置文件语法后退出（不启动服务器）
    #[arg(short, long, default_value_t = false)]
    test: bool,
}

fn main() {
    // 初始化日志（可通过 RUST_LOG 环境变量控制级别）
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    // 加载并解析配置文件
    let cfg = match config::loader::load_config(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            error!("配置文件加载失败: {}", e);
            std::process::exit(1);
        }
    };

    if cli.test {
        // -t 参数：仅测试配置，不启动
        info!("配置文件语法正确，共 {} 个站点", cfg.sites.len());
        return;
    }

    info!(
        "Sweety {} 正在启动，加载 {} 个站点",
        env!("CARGO_PKG_VERSION"),
        cfg.sites.len()
    );

    // 构建 Tokio 运行时
    let worker_threads = if cfg.global.worker_threads == 0 {
        num_cpus()
    } else {
        cfg.global.worker_threads
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .expect("Tokio 运行时构建失败");

    rt.block_on(async move {
        if let Err(e) = server::http::run(cfg).await {
            error!("服务器运行出错: {}", e);
            std::process::exit(1);
        }
    });
}

/// 获取 CPU 核心数（用于默认 worker 线程数）
fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}
