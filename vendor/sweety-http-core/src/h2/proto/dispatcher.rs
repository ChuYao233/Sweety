use core::{
    cmp, fmt,
    future::{Future, poll_fn},
    marker::PhantomData,
    net::SocketAddr,
    pin::{Pin, pin},
    task::{Context, Poll, ready},
    time::Duration,
};

use ::h2::{
    Ping, PingPong,
    ext::Protocol,
    server::{Connection, SendResponse},
};
use futures_core::stream::Stream;
use futures_util::stream::{FuturesUnordered, StreamExt};
use tracing::trace;
use sweety_io_compat::io::{AsyncRead, AsyncWrite};
use sweety_service::Service;
use sweety_collection::futures::{Select as _, SelectOutput};
use std::{rc::Rc, sync::Arc};

use crate::{
    body::BodySize,
    bytes::Bytes,
    date::{DateTime, DateTimeHandle},
    h2::{body::RequestBody, error::Error},
    http::{
        Extension, Request, RequestExt, Response, Version,
        header::{CONNECTION, CONTENT_LENGTH, DATE, HeaderMap, HeaderName, HeaderValue, TRAILER},
    },
    util::timer::KeepAlive,
};

/// Http/2 dispatcher
pub(crate) struct Dispatcher<'a, TlsSt, S, ReqB> {
    io: &'a mut Connection<TlsSt, Bytes>,
    addr: SocketAddr,
    is_tls: bool,
    keep_alive: Pin<&'a mut KeepAlive>,
    ka_dur: Duration,
    max_pending: usize,
    /// 单条连接最大请求数（0 = 不限制），达到后发 GOAWAY 优雅关闭
    /// 对标 Nginx keepalive_requests，强制客户端重建连接重新分散负载到各 worker
    max_requests: usize,
    service: Arc<S>,
    date: Rc<DateTimeHandle>,
    /// 底层 TCP socket raw fd（Linux 上用于 TCP_CORK，0 表示不可用）
    raw_fd: i32,
    _req_body: PhantomData<ReqB>,
}

impl<'a, TlsSt, S, ReqB, ResB, BE> Dispatcher<'a, TlsSt, S, ReqB>
where
    S: Service<Request<RequestExt<ReqB>>, Response = Response<ResB>> + 'static,
    S::Error: fmt::Debug,
    ResB: Stream<Item = Result<Bytes, BE>>,
    BE: fmt::Debug,
    TlsSt: AsyncRead + AsyncWrite + Unpin,
    ReqB: From<RequestBody> + 'static,
{
    pub(crate) fn new(
        io: &'a mut Connection<TlsSt, Bytes>,
        addr: SocketAddr,
        is_tls: bool,
        keep_alive: Pin<&'a mut KeepAlive>,
        ka_dur: Duration,
        max_pending: usize,
        max_requests: usize,
        service: Arc<S>,
        date: Rc<DateTimeHandle>,
        raw_fd: i32,
    ) -> Self {
        Self { io, addr, is_tls, keep_alive, ka_dur, max_pending, max_requests, service, date, raw_fd, _req_body: PhantomData }
    }

    async fn run_handler(
        req: Request<::h2::RecvStream>,
        respond: ::h2::server::SendResponse<Bytes>,
        service: Arc<S>,
        date: Rc<DateTimeHandle>,
        addr: SocketAddr,
        is_tls: bool,
        raw_fd: i32,
    ) -> Result<bool, ()> {
        // 检测 H2 extended CONNECT（RFC 8441）：:method=CONNECT + :protocol=websocket
        // h2 crate 把 :protocol 伪头存入 req.extensions()，检测后用 new_h2_ws 标记
        // 否则 is_h2_ws() 永远 false，CONNECT 请求被当作隧道返回 400，WS 握手失败
        let is_h2_ws = req.extensions().get::<Protocol>()
            .map(|p| p.as_str().eq_ignore_ascii_case("websocket"))
            .unwrap_or(false);
        let req = req.map(|body| {
            let ext = if is_h2_ws {
                Extension::new_h2_ws(addr, is_tls)
            } else {
                Extension::new(addr, is_tls)
            };
            RequestExt::from_parts(RequestBody::from(body).into(), ext)
        });
        let resp = match service.call(req).await {
            Ok(r) => r,
            Err(e) => { trace!("h2 service error: {:?}", e); return Ok(true); }
        };
        h2_handler(resp, respond, &date, raw_fd).await
    }

    pub(crate) async fn run(self) -> Result<(), Error<S::Error, BE>> {
        let Self { io, addr, is_tls, mut keep_alive, ka_dur, max_pending, max_requests, service, date, raw_fd, .. } = self;
        let mut req_count: usize = 0;

        let ping_pong = io.ping_pong().expect("first call to ping_pong should never fail");
        let deadline = date.now() + ka_dur;
        keep_alive.as_mut().update(deadline);

        let mut ping_pong = H2PingPong {
            on_flight: false,
            keep_alive: keep_alive.as_mut(),
            ping_pong,
            date: Rc::clone(&date),
            ka_dur,
        };

        // FuturesUnordered：在同一 task 里推进该连接所有并发 handler
        // 等价于 Nginx 事件循环在同一 epoll 线程里处理所有流——零 task switch 开销
        // 相比 spawn_local，避免了每请求的 tokio task 切换（对 1KB 短请求差异显著）
        let mut queue: FuturesUnordered<Pin<Box<dyn Future<Output = Result<bool, ()>>>>> = FuturesUnordered::new();

        enum Ev {
            Accept(Option<Result<(Request<::h2::RecvStream>, ::h2::server::SendResponse<Bytes>), ::h2::Error>>),
            QueueDone(Option<Result<bool, ()>>),
            Ping(Result<(), ::h2::Error>),
        }

        loop {
            let ev = if queue.is_empty() {
                match io.accept().select(&mut ping_pong).await {
                    SelectOutput::A(a) => Ev::Accept(a),
                    SelectOutput::B(r) => Ev::Ping(r),
                }
            } else {
                tokio::select! {
                    r = queue.next()  => Ev::QueueDone(r),
                    a = io.accept()   => Ev::Accept(a),
                    r = &mut ping_pong => Ev::Ping(r),
                }
            };

            match ev {
                Ev::QueueDone(Some(Ok(false))) => io.graceful_shutdown(),
                Ev::QueueDone(_) => {}

                Ev::Ping(Ok(())) => {
                    trace!("Connection keep-alive timeout. Shutting down");
                    while queue.next().await.is_some() {}
                    return Ok(());
                }
                Ev::Ping(Err(e)) => return Err(From::from(e)),

                Ev::Accept(None) => return Ok(()),
                Ev::Accept(Some(Err(e))) => return Err(e.into()),
                Ev::Accept(Some(Ok((req, respond)))) => {
                    if max_pending > 0 && queue.len() >= max_pending {
                        io.graceful_shutdown();
                    }
                    req_count += 1;
                    if max_requests > 0 && req_count >= max_requests {
                        io.graceful_shutdown();
                    }
                    queue.push(Box::pin(Self::run_handler(
                        req, respond, Arc::clone(&service), Rc::clone(&date), addr, is_tls, raw_fd,
                    )));
                }
            }
        }
    }
}

struct H2PingPong<'a> {
    on_flight: bool,
    keep_alive: Pin<&'a mut KeepAlive>,
    ping_pong: PingPong,
    date: Rc<DateTimeHandle>,
    ka_dur: Duration,
}

impl Future for H2PingPong<'_> {
    type Output = Result<(), ::h2::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        loop {
            if this.on_flight {
                match this.ping_pong.poll_pong(cx)? {
                    Poll::Ready(_) => {
                        this.on_flight = false;
                        let deadline = this.date.now() + this.ka_dur;
                        this.keep_alive.as_mut().update(deadline);
                        this.keep_alive.as_mut().reset();
                    }
                    Poll::Pending => return this.keep_alive.as_mut().poll(cx).map(|_| Ok(())),
                }
            } else {
                ready!(this.keep_alive.as_mut().poll(cx));
                this.ping_pong.send_ping(Ping::opaque())?;
                let deadline = this.date.now() + (this.ka_dur * 2);
                this.keep_alive.as_mut().update(deadline);
                this.on_flight = true;
            }
        }
    }
}

// 小文件内联阈值：≤ 32KB 的小文件 collect 后整块发送，消除流控等待和多次调度开销
// > 32KB 的中/大文件走消费者驱动的流式路径，按 H2 send window 容量按需读取
// 避免高并发时一次性占用过多内存（10000流×1MB=10GB → 10000流×32KB=320MB）
const SMALL_BODY_INLINE: usize = 32 * 1024;
// pread stream 每块大小（与 sendfile.rs 的 STREAM_CHUNK 对齐）
const CHUNK_SIZE: usize = 32 * 1024;
// 大文件流水线窗口：每流 reserve_capacity 的量
// 配合 max_send_buffer_size(32KB)，控制每流在 h2 内部缓冲上限
// Nginx 风格：小窗口 + 消费者驱动，高并发时自动回压（10000流×16KB=160MB）
const PIPELINE_WINDOW: usize = 16 * 1024;

/// Linux TCP_CORK：攒满再发，等价 Nginx tcp_nopush
/// 在 send_response 前 CORK，最后一帧 send_data 后 UNCORK
#[cfg(target_os = "linux")]
#[inline(always)]
#[allow(unsafe_code)]
fn set_tcp_cork(fd: i32, on: bool) {
    if fd <= 0 { return; }
    use std::os::unix::io::FromRawFd;
    // SAFETY: fd 来自已建立的 TLS 连接，生命周期由调用方保证有效
    // ManuallyDrop 确保函数返回时不会 close fd（不拥有所有权）
    let owned = std::mem::ManuallyDrop::new(unsafe { std::net::TcpStream::from_raw_fd(fd) });
    let _ = socket2::SockRef::from(&*owned).set_cork(on);
}

#[cfg(not(target_os = "linux"))]
#[inline(always)]
fn set_tcp_cork(_fd: i32, _on: bool) {}

async fn h2_handler<B, BE>(
    resp: Response<B>,
    mut respond: SendResponse<Bytes>,
    date: &DateTimeHandle,
    raw_fd: i32,
) -> Result<bool, ()>
where
    B: Stream<Item = Result<Bytes, BE>>,
    BE: fmt::Debug,
{
    let (res, body) = resp.into_parts();
    let mut res = Response::from_parts(res, ());
    *res.version_mut() = Version::HTTP_2;

    let body_size = BodySize::from_stream(&body);
    let is_eof = match body_size {
        BodySize::None   => true,
        BodySize::Stream => false,
        BodySize::Sized(n) => {
            if !res.headers().contains_key(CONTENT_LENGTH) {
                res.headers_mut().insert(CONTENT_LENGTH, HeaderValue::from(n));
            }
            n == 0
        }
    };

    // trailers 惰性分配：绝大多数响应无 trailer，跳过无条件 HeaderMap 分配
    let mut trailers: Option<HeaderMap> = None;
    while let Some(value) = res.headers_mut().remove(TRAILER) {
        let name = HeaderName::from_bytes(value.as_bytes()).unwrap();
        let value = match res.headers_mut().remove(name.clone()) {
            Some(v) => v,
            None => continue,
        };
        trailers.get_or_insert_with(|| HeaderMap::with_capacity(2)).append(name, value);
    }
    if !res.headers().contains_key(DATE) {
        let d = date.with_date(HeaderValue::from_bytes).unwrap();
        res.headers_mut().insert(DATE, d);
    }
    // Connection: close → 返回 false 通知 dispatcher graceful_shutdown
    let keep_alive = res.headers_mut().remove(CONNECTION)
        .map(|v| !v.as_bytes().eq_ignore_ascii_case(b"close"))
        .unwrap_or(true);

    // 无 body
    if is_eof {
        set_tcp_cork(raw_fd, true);
        let _ = respond.send_response(res, true);
        set_tcp_cork(raw_fd, false);
        return Ok(keep_alive);
    }

    // 小文件内联快路：collect 全部 body 后整块发送
    // ≤ 32KB：单块 Bytes（pread_exact）同步 Ready，零 await
    // 消除 H2 流控等待：整块 send_data 比分块流式减少多次 poll_capacity 调度往返
    if let BodySize::Sized(n) = body_size {
        if n as usize <= SMALL_BODY_INLINE {
            let data: Option<Bytes> = {
                let mut body = pin!(body);
                // 先尝试同步 poll（单块 Bytes body 永远同步 Ready，避免不必要的 await）
                let waker = futures_util::task::noop_waker();
                let mut cx = Context::from_waker(&waker);
                match body.as_mut().poll_next(&mut cx) {
                    Poll::Pending => {
                        // 异步 body（少见），直接 async await 第一块
                        poll_fn(|cx| body.as_mut().poll_next(cx)).await
                            .and_then(|r| r.ok())
                    }
                    Poll::Ready(None) => None,
                    Poll::Ready(Some(Err(_))) => None,
                    Poll::Ready(Some(Ok(first))) => {
                        // 检查是否还有更多 chunk
                        match body.as_mut().poll_next(&mut cx) {
                            Poll::Ready(None) => Some(first), // 唯一 chunk，零拷贝返回
                            Poll::Ready(Some(Ok(second))) => {
                                // 多 chunk 同步合并
                                let cap = n as usize;
                                let mut buf = crate::bytes::BytesMut::with_capacity(cap);
                                buf.extend_from_slice(&first);
                                buf.extend_from_slice(&second);
                                loop {
                                    match body.as_mut().poll_next(&mut cx) {
                                        Poll::Ready(Some(Ok(c))) => buf.extend_from_slice(&c),
                                        Poll::Ready(_) => break,
                                        Poll::Pending => {
                                            // stream 还有数据但需要 async，切换到 async collect
                                            while let Some(Ok(c)) = poll_fn(|cx| body.as_mut().poll_next(cx)).await {
                                                buf.extend_from_slice(&c);
                                            }
                                            break;
                                        }
                                    }
                                }
                                Some(buf.freeze())
                            }
                            Poll::Pending => {
                                // 第二块需要 async，先把第一块存着，async collect 剩余
                                let cap = n as usize;
                                let mut buf = crate::bytes::BytesMut::with_capacity(cap);
                                buf.extend_from_slice(&first);
                                while let Some(Ok(c)) = poll_fn(|cx| body.as_mut().poll_next(cx)).await {
                                    buf.extend_from_slice(&c);
                                }
                                Some(buf.freeze())
                            }
                            _ => Some(first),
                        }
                    }
                }
            };
            let has_trailers = trailers.is_some();
            set_tcp_cork(raw_fd, true);
            match data {
                None => { let _ = respond.send_response(res, true); }
                Some(data) => {
                    if let Ok(mut stream) = respond.send_response(res, false) {
                        // 先申请容量：初始流控窗口仅 65535，直接 send_data 超过窗口会被丢弃导致 EOF
                        let total = data.len();
                        stream.reserve_capacity(total);
                        let cap = stream.capacity();
                        if cap >= total {
                            // 窗口足够（小文件或对端窗口够大），直接发
                            let _ = stream.send_data(data, !has_trailers);
                        } else {
                            // 窗口不够（典型：100KB > 65535），需要分块等 WINDOW_UPDATE
                            // 先 UNCORK 让 HEADERS 刷到对端，对端才会发 WINDOW_UPDATE
                            set_tcp_cork(raw_fd, false);
                            let mut remaining = data;
                            loop {
                                let cap = stream.capacity();
                                if cap > 0 {
                                    let n = cmp::min(cap, remaining.len());
                                    let chunk = remaining.split_to(n);
                                    let eos = remaining.is_empty() && !has_trailers;
                                    let _ = stream.send_data(chunk, eos);
                                    stream.reserve_capacity(remaining.len());
                                    if remaining.is_empty() { break; }
                                } else {
                                    match poll_fn(|cx| stream.poll_capacity(cx)).await {
                                        None | Some(Err(_)) => {
                                            set_tcp_cork(raw_fd, false);
                                            return Ok(keep_alive);
                                        }
                                        Some(Ok(_)) => {}
                                    }
                                }
                            }
                            // 分块完成后不再需要重新 CORK（后续直接 UNCORK）
                        }
                        if let Some(t) = trailers { let _ = stream.send_trailers(t); }
                    }
                }
            }
            set_tcp_cork(raw_fd, false);
            return Ok(keep_alive);
        }
    }

    // 大文件/流式：消费者驱动发送（Nginx 风格）
    //
    // 核心原则：只有 H2 send window 有容量时才从磁盘读取下一块数据
    //   ① 先检查 capacity，无容量则等 WINDOW_UPDATE（不读磁盘，零内存增长）
    //   ② 有容量后才 poll_next 读取一块（pread 从 kernel page cache，几乎立即 Ready）
    //   ③ 按 capacity 分块发送，remaining 清空后再回 ①
    //
    // 内存保证：每流最多 1 个 chunk(32KB) + h2 buffer(32KB) = 64KB
    //   10,000 并发流 × 64KB = 640MB（vs 旧方案 10GB+）
    set_tcp_cork(raw_fd, true);
    let mut stream = match respond.send_response(res, false) {
        Ok(s) => s,
        Err(_) => { set_tcp_cork(raw_fd, false); return Ok(keep_alive); }
    };
    let mut body = pin!(body);

    // 预申请窗口：让 h2 crate 开始追踪流控需求
    stream.reserve_capacity(PIPELINE_WINDOW);

    loop {
        // ① 先确保有发送容量，没有就等 WINDOW_UPDATE（不读磁盘）
        if stream.capacity() == 0 {
            set_tcp_cork(raw_fd, false);
            match poll_fn(|cx| stream.poll_capacity(cx)).await {
                Some(Ok(_)) => { set_tcp_cork(raw_fd, true); }
                _ => return Ok(keep_alive),
            }
        }

        // ② 有容量了，读一块数据（文件：pread kernel page cache 几乎立即 Ready）
        let chunk = match poll_fn(|cx| body.as_mut().poll_next(cx)).await {
            None | Some(Err(_)) => break,
            // 空 chunk 跳过，不当做 EOF：WebSocket 空帧、流控屏障等场景下可能吸入 0 字节，
            // 若 break 则 h2 会发送 EOS 帧强制关闭流，导致 WebSocket 连接异常断开
            Some(Ok(c)) if c.is_empty() => continue,
            Some(Ok(c)) => c,
        };

        // ③ 按 capacity 分块发送
        let mut remaining = chunk;

        loop {
            let cap = stream.capacity();
            if cap > 0 {
                let n = cmp::min(cap, remaining.len());
                let to_send = remaining.split_to(n);
                let _ = stream.send_data(to_send, false);
                stream.reserve_capacity(PIPELINE_WINDOW);
                if remaining.is_empty() { break; }
            } else {
                // 窗口耗尽：UNCORK flush 已攒数据，等 WINDOW_UPDATE
                set_tcp_cork(raw_fd, false);
                match poll_fn(|cx| stream.poll_capacity(cx)).await {
                    Some(Ok(_)) => { set_tcp_cork(raw_fd, true); }
                    _ => return Ok(keep_alive),
                }
            }
        }
    }

    if let Some(t) = trailers {
        let _ = stream.send_trailers(t);
    } else {
        let _ = stream.send_data(Bytes::new(), true);
    }
    set_tcp_cork(raw_fd, false);

    Ok(keep_alive)
}
