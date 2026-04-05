//! `sweety run` —— 前台运行

use std::path::PathBuf;

use tracing::{error, info};
use sweety_lib::server::http::SweetyServer;

use crate::util::load_cfg_or_exit;

/// 提升进程 nofile soft/hard limit，避免高并发下 fd 耗尽
#[cfg(unix)]
fn raise_nofile_limit() {
    use std::mem::MaybeUninit;
    const TARGET: libc::rlim_t = 1_048_576;
    unsafe {
        let mut rl = MaybeUninit::<libc::rlimit>::uninit();
        if libc::getrlimit(libc::RLIMIT_NOFILE, rl.as_mut_ptr()) != 0 { return; }
        let mut rl = rl.assume_init();
        if rl.rlim_max != libc::RLIM_INFINITY && rl.rlim_max < TARGET {
            rl.rlim_max = TARGET;
        }
        let target_soft = if rl.rlim_max == libc::RLIM_INFINITY { TARGET } else { rl.rlim_max };
        if rl.rlim_cur < target_soft {
            rl.rlim_cur = target_soft;
            libc::setrlimit(libc::RLIMIT_NOFILE, &rl);
        }
    }
}

/// 将 panic 信息路由到 tracing error 日志，同时保留默认 stderr 输出
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
            *s
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.as_str()
        } else {
            "(unknown panic payload)"
        };
        let location = info.location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "(unknown location)".to_string());
        error!(
            panic.message = msg,
            panic.location = %location,
            "PANIC: {} at {}",
            msg, location,
        );
        default_hook(info);
    }));
}

pub fn cmd_run(config: &PathBuf) {
    #[cfg(unix)]
    raise_nofile_limit();

    let cfg = load_cfg_or_exit(config);
    // quinn_udp GSO 探测失败是无害的一次性日志，自动压制
    let base_filter = format!("{},quinn_udp=error", cfg.global.log_level);
    let log_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&base_filter));
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
    let (reload_filter, reload_handle) = tracing_subscriber::reload::Layer::new(log_filter);
    tracing_subscriber::registry()
        .with(reload_filter)
        .with(tracing_subscriber::fmt::layer())
        .init();
    sweety_lib::config::hot_reload::set_log_reload_handle(reload_handle);

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
