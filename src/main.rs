//! Sweety Web Server —— 程序入口
//!
//! 职责：全局分配器配置、rustls provider 初始化、CLI 解析、分发子命令。
//! 具体命令实现见各 `cmd::*` 子模块。

#[cfg(all(not(windows), feature = "jemalloc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

mod cli;
mod cmd;
mod util;

use clap::Parser;
use cli::{Cli, Commands};

fn main() {
    // instant-acme / reqwest 等传递依赖同时引入了 ring 和 aws-lc-rs，
    // rustls 无法自动选择，必须在最早点手动安装全局 provider
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let cli = Cli::parse();
    let config  = &cli.config;
    let pid_file = &cli.pid_file;

    match cli.command.unwrap_or(Commands::Run) {
        Commands::ApiDoc   => cmd::cmd_api_doc(),
        Commands::Version  => cmd::cmd_version(),
        Commands::Validate => cmd::validate::cmd_validate(config),
        Commands::Reload   => cmd::reload::cmd_reload(config),
        Commands::Run      => cmd::run::cmd_run(config),
        Commands::Start    => cmd::daemon::cmd_start(config, pid_file),
        Commands::Stop     => cmd::daemon::cmd_stop(pid_file),
        Commands::Restart  => {
            cmd::daemon::cmd_stop(pid_file);
            cmd::daemon::cmd_start(config, pid_file);
        }
    }
}
