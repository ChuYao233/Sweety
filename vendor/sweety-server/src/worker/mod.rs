mod shutdown;

use core::{any::Any, sync::atomic::AtomicBool, time::Duration};

use std::{io, rc::Rc, sync::Arc, thread};

use tokio::{sync::Notify, task::JoinHandle, time::sleep};
use tracing::{error, info};
use sweety_io_compat::net::Stream;
use sweety_service::{Service, ready::ReadyService};

use crate::net::ListenerDyn;

use self::shutdown::ShutdownHandle;

// erase Rc<S: ReadyService<_>> type and only use it for counting the reference counter of Rc.
pub(crate) type ServiceAny = Rc<dyn Any>;

/// 每次批量 accept 的最大连接数（对标 Nginx multi_accept on 的行为）
const ACCEPT_BATCH: usize = 256;

pub(crate) fn start<S, Req>(listener: &ListenerDyn, service: &Rc<S>) -> JoinHandle<()>
where
    S: ReadyService + Service<Req> + 'static,
    S::Ready: 'static,
    Req: TryFrom<Stream> + 'static,
{
    let listener = listener.clone();
    let service = service.clone();

    tokio::task::spawn_local(async move {
        loop {
            match listener.accept_dyn().await {
                Ok(stream) => {
                    // 接受后立即 spawn，service.ready() 在 task 内部等待
                    // 这样 accept 循环永不阻塞，QUIC endpoint 能持续消费 UDP 接收缓冲区，
                    // 避免 kernel 丢包导致 quinn 触发 333ms 重传定时器
                    let svc = service.clone();
                    tokio::task::spawn_local(async move {
                        let ready = svc.ready().await;
                        if let Ok(req) = Req::try_from(stream) {
                            let _ = svc.call(req).await;
                        }
                        drop(ready);
                    });

                    // 批量 accept：用非阻塞 try_accept_dyn 尽量多接受等待中的连接
                    // 对标 Nginx multi_accept on：一次事件后接受所有等待的连接
                    // 注意：QUIC 的 endpoint.accept() 没有新连接时不返回 WouldBlock 而是 Pending，
                    // 必须用 try_accept_dyn（只 poll 一次）避免阻塞已接受连接的 task 调度
                    for _ in 0..ACCEPT_BATCH {
                        match listener.try_accept_dyn() {
                            Ok(Some(stream)) => {
                                let svc = service.clone();
                                tokio::task::spawn_local(async move {
                                    let ready = svc.ready().await;
                                    if let Ok(req) = Req::try_from(stream) {
                                        let _ = svc.call(req).await;
                                    }
                                    drop(ready);
                                });
                            }
                            Ok(None) => break,
                            // 没有更多连接等待，退出批量 accept
                            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                            Err(ref e) if connection_error(e) => continue,
                            Err(ref e) if fatal_error(e) => return,
                            Err(ref e) if os_error(e) => {
                                error!("Error accepting connection: {e}");
                                sleep(Duration::from_secs(1)).await;
                                break;
                            }
                            Err(_) => return,
                        }
                    }
                }
                Err(ref e) if connection_error(e) => continue,
                Err(ref e) if fatal_error(e) => return,
                Err(ref e) if os_error(e) => {
                    error!("Error accepting connection: {e}");
                    sleep(Duration::from_secs(1)).await;
                }
                Err(_) => return,
            }
        }
    })
}

pub(crate) async fn wait_for_stop(
    handles: Vec<JoinHandle<()>>,
    services: Vec<ServiceAny>,
    shutdown_timeout: Duration,
    is_graceful_shutdown: &AtomicBool,
    stop_notify: Arc<Notify>,
) {
    with_worker_name_str(|name| info!("Started {name}"));

    let shutdown_handle = ShutdownHandle::new(shutdown_timeout, services, is_graceful_shutdown);

    // 事件驱动等待停止信号（Server::stop 调用 notify_waiters 唤醒）
    // ForceStop 路径直接 process::exit(0)，不会到达这里
    stop_notify.notified().await;

    // 收到 GracefulStop：abort accept loop（不再接受新连接），然后排空活跃连接
    for handle in &handles {
        handle.abort();
    }
    for handle in handles {
        let _ = handle.await; // 忽略 JoinError::Cancelled
    }

    shutdown_handle.shutdown().await;
}

#[cold]
#[inline(never)]
fn with_worker_name_str<F, O>(func: F) -> O
where
    F: FnOnce(&str) -> O,
{
    match thread::current().name() {
        Some(name) => func(name),
        None => func("sweety-server-worker"),
    }
}

/// This function defines errors that are per-connection. Which basically
/// means that if we get this error from `accept()` system call it means
/// next connection might be ready to be accepted.
///
/// All other errors will incur a timeout before next `accept()` is performed.
/// The timeout is useful to handle resource exhaustion errors like ENFILE
/// and EMFILE. Otherwise, could enter into tight loop.
fn connection_error(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::ConnectionRefused
        || e.kind() == io::ErrorKind::ConnectionAborted
        || e.kind() == io::ErrorKind::ConnectionReset
}

/// fatal error that can not be recovered.
fn fatal_error(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::BrokenPipe
}

/// std::io::Error is a widely used type through dependencies and this method is
/// used to tell the difference of os io error from dependency crate io error.
/// (for example tokio use std::io::Error to hint runtime shutdown)
fn os_error(e: &io::Error) -> bool {
    e.raw_os_error().is_some()
}
