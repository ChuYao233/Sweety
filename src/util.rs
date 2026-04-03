//! 跨命令共享的辅助函数

use std::path::PathBuf;

/// 加载配置，失败则打印错误并退出进程
pub fn load_cfg_or_exit(config: &PathBuf) -> sweety_lib::config::model::AppConfig {
    match sweety_lib::config::loader::load_config(config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[ERROR] 配置文件加载失败: {:#}", e);
            std::process::exit(1);
        }
    }
}

/// 初始化仅输出到 stderr 的最简日志（用于非 run 命令）
pub fn init_stderr_log() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info")
        .with_writer(std::io::stderr)
        .try_init();
}
