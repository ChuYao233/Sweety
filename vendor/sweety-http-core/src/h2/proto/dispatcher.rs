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
    service: Arc<S>,
    date: Rc<DateTimeHandle>,
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
        service: Arc<S>,
        date: Rc<DateTimeHandle>,
    ) -> Self {
        Self { io, addr, is_tls, keep_alive, ka_dur, max_pending, service, date, _req_body: PhantomData }
    }

    async fn run_handler(
        req: Request<::h2::RecvStream>,
        respond: ::h2::server::SendResponse<Bytes>,
        service: Arc<S>,
        date: Rc<DateTimeHandle>,
        addr: SocketAddr,
        is_tls: bool,
    ) -> Result<bool, ()> {
        let req = req.map(|body| {
            RequestExt::from_parts(RequestBody::from(body).into(), Extension::new(addr, is_tls))
        });
        let resp = match service.call(req).await {
            Ok(r) => r,
            Err(e) => { trace!("h2 service error: {:?}", e); return Ok(true); }
        };
        h2_handler(resp, respond, &date).await
    }

    pub(crate) async fn run(self) -> Result<(), Error<S::Error, BE>> {
        let Self { io, addr, is_tls, mut keep_alive, ka_dur, max_pending, service, date, .. } = self;

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

        let mut queue: FuturesUnordered<Pin<Box<dyn Future<Output = Result<bool, ()>>>>> = FuturesUnordered::new();

        // select! 只负责等事件，不做 .await 副作用
        // 用枚举区分三路事件，accept 结果移到 select! 外处理（做首次 poll）
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
                    queue.push(Box::pin(Self::run_handler(
                        req, respond, Arc::clone(&service), Rc::clone(&date), addr, is_tls,
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

// 小文件内联阈值：≤ 32KB（H2 初始窗口 65535，32KB 以内无需等窗口）
const SMALL_BODY_INLINE: usize = 32 * 1024;
// mmap stream 每块大小（与 sendfile.rs 的 STREAM_CHUNK 对齐）
const CHUNK_SIZE: usize = 256 * 1024;
// 大文件流水线窗口：允许提前读入并排队发送的最大字节数
// 等于 max_send_buffer_size(1MB)，超过此量才等 poll_capacity 背压
// 这样读文件和网络发送完全重叠，消除串行 RTT 等待
const PIPELINE_WINDOW: usize = 1024 * 1024;

async fn h2_handler<B, BE>(
    resp: Response<B>,
    mut respond: SendResponse<Bytes>,
    date: &DateTimeHandle,
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
        let _ = respond.send_response(res, true);
        return Ok(keep_alive);
    }

    // 小文件快路：同步 collect body（ResponseBody::Bytes 的 poll_next 永远同步 Ready）
    // 不使用 poll_fn(..).await 避免每次 chunk 都交还调度器
    if let BodySize::Sized(n) = body_size {
        if n as usize <= SMALL_BODY_INLINE {
            let data: Option<Bytes> = {
                let mut body = pin!(body);
                // noop waker：只做同步 poll，不注册真实唤醒
                let waker = futures_util::task::noop_waker();
                let mut cx = Context::from_waker(&waker);
                match body.as_mut().poll_next(&mut cx) {
                    Poll::Pending => {
                        // 极少数情况 body 异步，降级到 poll_fn await
                        poll_fn(|cx| body.as_mut().poll_next(cx)).await
                            .and_then(|r| r.ok())
                    }
                    Poll::Ready(None) => None,
                    Poll::Ready(Some(Err(_))) => None,
                    Poll::Ready(Some(Ok(first))) => {
                        // 检查是否还有更多 chunk（绝大多数情况 Bytes body 只有一个 chunk）
                        match body.as_mut().poll_next(&mut cx) {
                            Poll::Ready(None) => Some(first), // 唯一 chunk，零拷贝
                            Poll::Ready(Some(Ok(second))) => {
                                // 多 chunk，合并（少见路径）
                                let mut buf = crate::bytes::BytesMut::with_capacity(first.len() + second.len());
                                buf.extend_from_slice(&first);
                                buf.extend_from_slice(&second);
                                loop {
                                    match body.as_mut().poll_next(&mut cx) {
                                        Poll::Ready(Some(Ok(c))) => buf.extend_from_slice(&c),
                                        _ => break,
                                    }
                                }
                                Some(buf.freeze())
                            }
                            _ => Some(first),
                        }
                    }
                }
            };
            let has_trailers = trailers.is_some();
            match data {
                None => { let _ = respond.send_response(res, true); }
                Some(data) => {
                    if let Ok(mut stream) = respond.send_response(res, false) {
                        let _ = stream.send_data(data, !has_trailers);
                        if let Some(t) = trailers { let _ = stream.send_trailers(t); }
                    }
                }
            }
            return Ok(keep_alive);
        }
    }

    // 大文件/流式：流水线发送
    //
    // 原理：reserve_capacity(PIPELINE_WINDOW) 预申请大窗口，h2 crate 持有发送队列
    //   capacity() > 0 时直接发，无需 await（零延迟）
    //   capacity() == 0 时 poll_capacity 等下一个 WINDOW_UPDATE，期间 tokio 调度其他任务
    //   读文件（mmap 纯内存）与网络发送重叠，消除串行 RTT 等待
    let mut stream = match respond.send_response(res, false) {
        Ok(s) => s,
        Err(_) => return Ok(keep_alive),
    };
    let mut body = pin!(body);

    // 预申请大窗口：让 h2 crate 立即开始追踪，尽早触发对端 WINDOW_UPDATE
    stream.reserve_capacity(PIPELINE_WINDOW);

    loop {
        // 读一块文件（mmap：纯内存，几乎立即 Ready）
        let chunk = match poll_fn(|cx| body.as_mut().poll_next(cx)).await {
            None | Some(Err(_)) => break,
            Some(Ok(c)) if c.is_empty() => break,
            Some(Ok(c)) => c,
        };

        let mut remaining = chunk;

        loop {
            let cap = stream.capacity();
            if cap > 0 {
                // 有窗口：同步发送，不 await
                let n = cmp::min(cap, remaining.len());
                let to_send = remaining.split_to(n);
                let _ = stream.send_data(to_send, false);
                // 维持 reserve 请求，让 h2 crate 持续追踪窗口需求
                stream.reserve_capacity(PIPELINE_WINDOW);
                if remaining.is_empty() { break; }
            } else {
                // 窗口耗尽：等 WINDOW_UPDATE（此时 tokio 可调度其他连接）
                match poll_fn(|cx| stream.poll_capacity(cx)).await {
                    Some(Ok(_)) => {} // 有新窗口，回到顶部重试
                    _ => {
                        // 连接/流已关闭（RST 或 GOAWAY）
                        return Ok(keep_alive);
                    }
                }
            }
        }
    }

    if let Some(t) = trailers {
        let _ = stream.send_trailers(t);
    } else {
        let _ = stream.send_data(Bytes::new(), true);
    }

    Ok(keep_alive)
}
