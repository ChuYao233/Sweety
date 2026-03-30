//! 配置热重载模块
//! 使用 notify 监听文件系统变更，防抖后重新解析配置并通过 tokio::watch 广播

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::watch;
use tracing::{error, info, warn};

use super::{loader::load_config, model::AppConfig};

/// 启动配置热重载监听器
///
/// 返回一个 `watch::Receiver`，持有最新的 `Arc<AppConfig>`。
/// 当配置文件变更且解析成功时，Receiver 会收到新配置。
pub async fn start_watcher(
    config_path: PathBuf,
    initial_config: AppConfig,
) -> Result<watch::Receiver<Arc<AppConfig>>> {
    let (tx, rx) = watch::channel(Arc::new(initial_config));

    let path_clone = config_path.clone();
    tokio::spawn(async move {
        if let Err(e) = watch_loop(path_clone, tx).await {
            error!("配置热重载监听器异常退出: {}", e);
        }
    });

    info!("配置热重载已启动，监听文件: {}", config_path.display());
    Ok(rx)
}

/// 文件监听主循环（运行在后台 task 中）
async fn watch_loop(config_path: PathBuf, tx: watch::Sender<Arc<AppConfig>>) -> Result<()> {
    // 使用 std channel 接收 notify 事件（notify 是同步 API）
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    // 创建文件系统监听器
    let mut watcher: RecommendedWatcher = {
        let etx = event_tx.clone();
        notify::recommended_watcher(move |res: notify::Result<Event>| match res {
            Ok(event) => {
                // 只关心内容修改和重命名（编辑器保存时可能触发 rename）
                if matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    let _ = etx.send(());
                }
            }
            Err(e) => warn!("文件监听事件错误: {}", e),
        })?
    };

    // 监听配置文件所在目录（监听目录比监听单文件更可靠）
    let watch_dir = config_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    watcher.watch(watch_dir, RecursiveMode::NonRecursive)?;

    loop {
        // 等待首个事件
        if event_rx.recv().await.is_none() {
            break;
        }
        // 防抖：等待 500ms，合并短时间内的多个事件
        tokio::time::sleep(Duration::from_millis(500)).await;
        // 排空队列中堆积的事件
        while event_rx.try_recv().is_ok() {}

        // 重新加载配置
        match load_config(&config_path) {
            Ok(new_cfg) => {
                info!("配置文件已变更，热重载成功，共 {} 个站点", new_cfg.sites.len());
                let _ = tx.send(Arc::new(new_cfg));
            }
            Err(e) => {
                error!("配置热重载失败（配置语法错误），继续使用旧配置: {}", e);
            }
        }
    }

    Ok(())
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn test_watcher_starts_without_error() {
        let mut f = tempfile::Builder::new()
            .suffix(".toml")
            .tempfile()
            .unwrap();
        writeln!(f, "[[sites]]\nname = \"demo\"\nserver_name = [\"localhost\"]").unwrap();

        let cfg = load_config(f.path()).unwrap();
        let rx = start_watcher(f.path().to_path_buf(), cfg).await.unwrap();
        // 初始值应正确
        assert_eq!(rx.borrow().sites[0].name, "demo");
    }
}
