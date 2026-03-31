mod shutdown;

use core::{any::Any, sync::atomic::AtomicBool, time::Duration};

use std::{io, rc::Rc, thread};

use tokio::{task::JoinHandle, time::sleep};
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
            // 等待服务就绪（背压控制）
            let ready = service.ready().await;

            match listener.accept_dyn().await {
                Ok(stream) => {
                    if let Ok(req) = TryFrom::try_from(stream) {
                        let svc = service.clone();
                        tokio::task::spawn_local(async move {
                            let _ = svc.call(req).await;
                            drop(ready);
                        });
                    } else {
                        drop(ready);
                    }

                    // 批量 accept：在同一 poll 周期内尽量多接受连接，减少 syscall 次数
                    // 对标 Nginx multi_accept on：一次 epoll 事件后接受所有等待的连接
                    for _ in 0..ACCEPT_BATCH {
                        let ready = service.ready().await;
                        match listener.accept_dyn().await {
                            Ok(stream) => {
                                if let Ok(req) = TryFrom::try_from(stream) {
                                    let svc = service.clone();
                                    tokio::task::spawn_local(async move {
                                        let _ = svc.call(req).await;
                                        drop(ready);
                                    });
                                } else {
                                    drop(ready);
                                }
                            }
                            // 没有更多连接等待，退出批量 accept
                            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                                drop(ready);
                                break;
                            }
                            Err(ref e) if connection_error(e) => {
                                drop(ready);
                                continue;
                            }
                            Err(ref e) if fatal_error(e) => return,
                            Err(ref e) if os_error(e) => {
                                error!("Error accepting connection: {e}");
                                drop(ready);
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
) {
    with_worker_name_str(|name| info!("Started {name}"));

    let shutdown_handle = ShutdownHandle::new(shutdown_timeout, services, is_graceful_shutdown);

    for handle in handles {
        handle
            .await
            .unwrap_or_else(|e| with_worker_name_str(|name| error!("{name} exit on error: {e}")));
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
