mod future;
mod handle;
mod service;

pub use self::{future::ServerFuture, handle::ServerHandle};

pub(crate) use self::service::{IntoServiceObj, ServiceObj};

use std::{
    io, mem,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use tokio::{
    runtime::Runtime,
    sync::mpsc::{UnboundedReceiver, UnboundedSender},
};

use tracing::{error, info};
use crate::{builder::Builder, worker};

pub struct Server {
    is_graceful_shutdown: Arc<AtomicBool>,
    tx_cmd: UnboundedSender<Command>,
    rx_cmd: UnboundedReceiver<Command>,
    rt: Option<Runtime>,
    worker_join_handles: Vec<thread::JoinHandle<io::Result<()>>>,
}

impl Server {
    #[cfg(target_family = "wasm")]
    pub fn new(builder: Builder) -> io::Result<Self> {
        let Builder {
            listeners,
            factories,
            shutdown_timeout,
            on_worker_start,
            ..
        } = builder;

        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;

        let fut = async {
            listeners
                .into_iter()
                .flat_map(|(name, listeners)| listeners.into_iter().map(move |l| l().map(|l| (name.to_owned(), l))))
                .collect::<Result<Vec<_>, io::Error>>()
        };

        let listeners = rt.block_on(fut)?;

        let is_graceful_shutdown = Arc::new(AtomicBool::new(false));

        let on_start_fut = on_worker_start();

        let fut = async {
            on_start_fut.await;

            let mut handles = Vec::new();
            let mut services = Vec::new();

            for (name, factory) in factories.iter() {
                let (h, s) = factory
                    .call((name, &listeners))
                    .await
                    .map_err(|_| io::Error::from(io::ErrorKind::Other))?;
                handles.extend(h);
                services.push(s);
            }

            worker::wait_for_stop(handles, services, shutdown_timeout, &is_graceful_shutdown).await;

            Ok::<_, io::Error>(())
        };

        rt.block_on(tokio::task::LocalSet::new().run_until(fut))?;

        unreachable!("")
    }

    #[cfg(not(target_family = "wasm"))]
    pub fn new(builder: Builder) -> io::Result<Self> {
        let Builder {
            server_threads,
            worker_threads,
            worker_max_blocking_threads,
            listeners,
            factories,
            shutdown_timeout,
            on_worker_start,
            ..
        } = builder;

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            // This worker threads is only used for accepting connections.
            // sweety-server worker does not run task on them.
            .worker_threads(server_threads)
            .build()?;

        // listeners 现在是 Arc<Fn()> 工厂字典，不在主线程里 bind
        // 将工厂字典包成 Arc 传给每个 worker
        let listeners = Arc::new(listeners);

        let is_graceful_shutdown = Arc::new(AtomicBool::new(false));
        let is_graceful_shutdown2 = is_graceful_shutdown.clone();

        // on_worker_start / factories / shutdown 需要跨多个 worker 线程共享，用 Arc 包装
        let on_worker_start = Arc::new(on_worker_start);
        let factories       = Arc::new(factories);

        let worker_handles = thread::Builder::new()
            .name(String::from("sweety-server-worker-shared-scope"))
            .spawn(move || {
                let is_graceful_shutdown = is_graceful_shutdown2;

                // TODO: wait for startup error(including panic) and return as io::Error on call site.
                // currently the error only show when shared scope thread is joined with handle.
                info!("sweety-server: 启动 {worker_threads} 个 worker 线程");
                thread::scope(|scope| {
                    for idx in 0..worker_threads {
                        let thread             = thread::Builder::new().name(format!("sweety-server-worker-{idx}"));
                        let listeners          = Arc::clone(&listeners);
                        let factories          = Arc::clone(&factories);
                        let on_worker_start    = Arc::clone(&on_worker_start);
                        let is_graceful_shutdown = Arc::clone(&is_graceful_shutdown);

                        let task = move || async move {
                            on_worker_start().await;

                            // SO_REUSEPORT per-worker bind：每个 worker 调用工厂函数，各自 bind 独立 fd
                            // 内核通过 SO_REUSEPORT 把连接平均分散到各 worker
                            let worker_listeners: Vec<(String, crate::net::ListenerDyn)> = {
                                let mut v = Vec::new();
                                for (name, factories) in listeners.iter() {
                                    for factory in factories {
                                        match factory() {
                                            Ok(l) => {
                                                info!("worker-{idx}: listener [{name}] bind 成功");
                                                v.push((name.clone(), l));
                                            }
                                            Err(e) => {
                                                error!("worker-{idx}: listener [{name}] bind 失败: {e}");
                                                return;
                                            }
                                        }
                                    }
                                }
                                v
                            };

                            let mut handles = Vec::new();
                            let mut services = Vec::new();

                            for (name, factory) in factories.iter() {
                                match factory.call((name, &worker_listeners)).await {
                                    Ok((h, s)) => {
                                        handles.extend(h);
                                        services.push(s);
                                    }
                                    Err(_) => return,
                                }
                            }

                            worker::wait_for_stop(handles, services, shutdown_timeout, &is_graceful_shutdown).await;
                        };

                        #[cfg(not(feature = "io-uring"))]
                        {
                            let rt = tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .max_blocking_threads(worker_max_blocking_threads)
                                // blocking thread 空闲 60s 后回收，避免长期占用系统线程
                                .thread_keep_alive(std::time::Duration::from_secs(60))
                                .build()?;

                            thread.spawn_scoped(scope, move || {
                                rt.block_on(tokio::task::LocalSet::new().run_until(task()))
                            })?;
                        }

                        #[cfg(feature = "io-uring")]
                        {
                            thread.spawn_scoped(scope, move || {
                                let _ = worker_max_blocking_threads;
                                tokio_uring::start(task())
                            })?;
                        }
                    }

                    Ok(())
                })
            })?;

        let (tx_cmd, rx_cmd) = tokio::sync::mpsc::unbounded_channel();

        Ok(Self {
            is_graceful_shutdown,
            tx_cmd,
            rx_cmd,
            rt: Some(rt),
            worker_join_handles: vec![worker_handles],
        })
    }

    pub(crate) fn stop(&mut self, graceful: bool) {
        if let Some(rt) = self.rt.take() {
            self.is_graceful_shutdown.store(graceful, Ordering::SeqCst);
            rt.shutdown_background();
            mem::take(&mut self.worker_join_handles).into_iter().for_each(|handle| {
                // 安全处理 join 错误，不 panic
                if let Err(e) = handle.join() {
                    tracing::warn!("worker thread exited with error: {:?}", e);
                }
            });
        }
    }
}

enum Command {
    GracefulStop,
    ForceStop,
}
