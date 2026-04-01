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
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;
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

// ── per-connection 写命令（WriteCmd）──────────────────────────────────────────
//
// 架构：Go net/http2 writeScheduler / Nginx event loop 单写线程模型
//
// 问题：h2 crate Connection 内部用 Arc<Mutex> 保护写状态，
//       500 个 stream 并发调 send_response() 时锁竞争严重，
//       HEADERS 发送延迟 → "awaiting headers timeout"
//
// 解法：每个 TCP 连接一个 writer loop（在 LocalSet 同一线程里运行），
//       所有 stream 的写操作通过 mpsc channel 排队，writer 串行执行：
//       1. send_response(HEADERS)
//       2. send_data(DATA) - 小文件内联，大文件通过 oneshot 返回 SendStream
//
// SendResponse<B>: !Send，所以必须在同一线程（LocalSet）内流转。

/// handler task → writer loop 的写命令
enum WriteCmd {
    /// 发送 HEADERS 帧（+ 可选内联 body）
    ///
    /// - `respond`     : 来自 io.accept() 的 SendResponse，writer 用它调 send_response
    /// - `res`         : 已构造好的响应头（不含 body）
    /// - `inline_body` : 小文件（<=SMALL_BODY_INLINE）已收集好的 body，None=大文件
    /// - `trailers`    : 响应 trailers（小文件随 HEADERS 一起处理）
    /// - `stream_tx`   : 大文件时 writer 通过此 oneshot 返回 SendStream，handler 继续发 DATA
    Headers {
        respond:      SendResponse<Bytes>,
        res:          Response<()>,
        /// true = 无 body（send_response 时 end_stream=true，不需要 DATA 帧）
        /// false = 有 body（小文件走 inline_body，大文件走 stream_tx）
        end_stream:   bool,
        inline_body:  Option<Bytes>,
        trailers:     HeaderMap,
        stream_tx:    oneshot::Sender<::h2::SendStream<Bytes>>,
    },
}

/// writer loop：批量处理连接级写操作（write batching）
///
/// 批量策略：先 recv() 等第一个 cmd，立即 try_recv() drain 其余就绪 cmd，
/// 批量 send_response 写入 h2 发送缓冲区，Connection::poll_send 统一 flush
/// → N 个 stream 的 HEADERS 在同一次 syscall 里发出，减少 N-1 次 write()
async fn conn_writer_loop(mut rx: mpsc::UnboundedReceiver<WriteCmd>) {
    let mut batch: Vec<WriteCmd> = Vec::with_capacity(WRITER_BATCH_SIZE);

    loop {
        // 阻塞等第一个 cmd
        match rx.recv().await {
            Some(cmd) => batch.push(cmd),
            None => break,
        }
        // 非阻塞 drain 剩余就绪 cmd（最多 WRITER_BATCH_SIZE）
        while batch.len() < WRITER_BATCH_SIZE {
            match rx.try_recv() {
                Ok(cmd) => batch.push(cmd),
                Err(_) => break,
            }
        }
        // 处理批次：所有 send_response 写入缓冲区，由 accept loop 驱动统一 flush
        for cmd in batch.drain(..) {
            match cmd {
                WriteCmd::Headers { mut respond, res, end_stream, inline_body, trailers, stream_tx } => {
                    if end_stream {
                        let _ = respond.send_response(res, true);
                        continue;
                    }
                    let mut stream = match respond.send_response(res, false) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };
                    if let Some(bytes) = inline_body {
                        if !bytes.is_empty() {
                            let len = bytes.len();
                            stream.reserve_capacity(len);
                            match poll_fn(|cx| stream.poll_capacity(cx)).await {
                                Some(Ok(cap)) if cap > 0 => {
                                    let _ = stream.send_data(bytes.slice(..cmp::min(cap, len)), false);
                                }
                                _ => {}
                            }
                        }
                        let _ = stream.send_trailers(trailers);
                    } else {
                        let _ = stream_tx.send(stream);
                    }
                }
            }
        }
    }
}

// 单批次最多处理 WriteCmd 数量：32 平衡吞吐与尾延迟
const WRITER_BATCH_SIZE: usize = 32;

/// Http/2 dispatcher
pub(crate) struct Dispatcher<'a, TlsSt, S, ReqB> {
    io: &'a mut Connection<TlsSt, Bytes>,
    addr: SocketAddr,
    is_tls: bool,
    keep_alive: Pin<&'a mut KeepAlive>,
    ka_dur: Duration,
    /// 应用级 pending 限制（0 = 不限制），超限时发 GOAWAY 优雅拒绝新流
    max_pending: usize,
    service: Arc<S>,
    date: Rc<DateTimeHandle>,
    _req_body: PhantomData<ReqB>,
}

impl<'a, TlsSt, S, ReqB, ResB, BE> Dispatcher<'a, TlsSt, S, ReqB>
where
    S: Service<Request<RequestExt<ReqB>>, Response = Response<ResB>> + 'static,
    S::Error: fmt::Debug,
    ResB: Stream<Item = Result<Bytes, BE>> + Send + 'static,
    BE: fmt::Debug + Send + 'static,
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
        Self {
            io,
            addr,
            is_tls,
            keep_alive,
            ka_dur,
            max_pending,
            service,
            date,
            _req_body: PhantomData,
        }
    }

    pub(crate) async fn run(self) -> Result<(), Error<S::Error, BE>> {
        let Self {
            io,
            addr,
            is_tls,
            mut keep_alive,
            ka_dur,
            max_pending,
            service,
            date,
            ..
        } = self;

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

        // ── per-connection writer channel ──────────────────────────────────────
        // writer loop 在 LocalSet 内和 accept loop 并发运行，但在同一线程上，
        // 保证 SendResponse<!Send> 和 SendStream<!Send> 不跨线程。
        // 所有 stream 的 HEADERS 写操作串行经过此 channel，消除锁竞争。
        let (write_tx, write_rx) = mpsc::unbounded_channel::<WriteCmd>();

        // connection-level in-flight 计数（仅监控，不限流）
        // 流控依赖协议层 max_concurrent_streams，不在应用层 RST
        let pending_count = Rc::new(std::cell::Cell::new(0usize));

        let local = LocalSet::new();

        // writer loop：唯一操作 Connection 写侧的执行上下文
        local.spawn_local(conn_writer_loop(write_rx));

        // accept loop：只 accept 新 stream，spawn_local per-stream handler
        let accept_result: Result<(), Error<S::Error, BE>> = local.run_until(async {
            loop {
                match io.accept().select(&mut ping_pong).await {
                    SelectOutput::A(Some(Ok((req, respond)))) => {
                        // RFC 8441：在 map 之前先读 extensions，避免 req 同时被 move 和 borrow
                        let is_h2_ws = req.extensions().get::<::h2::ext::Protocol>()
                            .map(|p| p.as_str().eq_ignore_ascii_case("websocket"))
                            .unwrap_or(false);
                        let req = req.map(|body| {
                            let ext = if is_h2_ws { Extension::new_h2_ws(addr, is_tls) } else { Extension::new(addr, is_tls) };
                            let body = ReqB::from(RequestBody::from(body));
                            RequestExt::from_parts(body, ext)
                        });

                        let write_tx = write_tx.clone();
                        let svc = Arc::clone(&service);
                        let dt  = Rc::clone(&date);
                        let pc  = Rc::clone(&pending_count);
                        // 应用级背压：超过 max_pending 时发 GOAWAY，优雅拒绝新流
                        // max_pending=0 表示不限制，依赖协议层 max_concurrent_streams
                        if max_pending > 0 && pc.get() >= max_pending {
                            io.graceful_shutdown();
                            // 仍然处理已 accept 的这个流，不丢弃
                        }
                        pc.set(pc.get() + 1);
                        tokio::task::spawn_local(async move {
                            h2_handler(svc.call(req), respond, &*dt, write_tx).await;
                            pc.set(pc.get() - 1);
                        });
                    }
                    SelectOutput::B(Ok(())) => {
                        trace!("Connection keep-alive timeout. Shutting down");
                        return Ok(());
                    }
                    SelectOutput::B(Err(e)) => return Err(From::from(e)),
                    SelectOutput::A(None) => {
                        trace!("Connection closed by remote. Shutting down");
                        break;
                    }
                    SelectOutput::A(Some(Err(e))) => return Err(From::from(e)),
                }
            }
            Ok(())
        }).await;

        // write_tx drop → writer loop channel 关闭 → writer loop 自然退出
        drop(write_tx);

        accept_result
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
                // When have on flight ping pong. poll pong and and keep alive timer.
                // on success pong received update keep alive timer to determine the next timing of
                // ping pong.
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
                // When there is no on flight ping pong. keep alive timer is used to wait for next
                // timing of ping pong. Therefore at this point it serves as an interval instead.

                ready!(this.keep_alive.as_mut().poll(cx));

                this.ping_pong.send_ping(Ping::opaque())?;

                // pong 等待窗口 = ka_dur * 2：足够跨越一次 RTT，又不会让僵尸连接长时间占用资源
                // Nginx keepalive_timeout 行为：超时后立即关闭，此处对齐
                let deadline = this.date.now() + (this.ka_dur * 2);

                this.keep_alive.as_mut().update(deadline);

                this.on_flight = true;
            }
        }
    }
}

/// per-stream handler（在 LocalSet 内 spawn_local）
///
/// 职责：
/// 1. service.call() 获取响应
/// 2. 构造响应头元数据
/// 3. 收集小文件 body（在内存，通常同步）
/// 4. 把 WriteCmd::Headers 发给 conn_writer_loop，handler 立即继续
/// 5. 大文件：等 writer loop 返回 SendStream，然后流式发 DATA
///
/// HEADERS 发送完全由 writer loop 串行处理，消除并发锁竞争
async fn h2_handler<Fut, B, SE, BE>(
    fut: Fut,
    respond: SendResponse<Bytes>,
    date: &DateTimeHandle,
    write_tx: mpsc::UnboundedSender<WriteCmd>,
)
where
    Fut: Future<Output = Result<Response<B>, SE>>,
    B: Stream<Item = Result<Bytes, BE>> + 'static,
    BE: fmt::Debug + 'static,
    SE: fmt::Debug,
{
    // 1. 调用 service，获取响应
    let resp = match fut.await {
        Ok(r) => r,
        Err(e) => {
            trace!("h2 service error: {:?}", e);
            return;
        }
    };

    let (res, body) = resp.into_parts();
    let mut res = Response::from_parts(res, ());
    *res.version_mut() = Version::HTTP_2;

    // 2. 构造响应头元数据
    let body_size = BodySize::from_stream(&body);
    let is_eof = match body_size {
        BodySize::None  => { debug_assert!(!res.headers().contains_key(CONTENT_LENGTH)); true  }
        BodySize::Stream => { debug_assert!(!res.headers().contains_key(CONTENT_LENGTH)); false }
        BodySize::Sized(n) => {
            if !res.headers().contains_key(CONTENT_LENGTH) {
                res.headers_mut().insert(CONTENT_LENGTH, HeaderValue::from(n));
            }
            n == 0
        }
    };

    let mut trailers = HeaderMap::with_capacity(0);
    while let Some(value) = res.headers_mut().remove(TRAILER) {
        let name = HeaderName::from_bytes(value.as_bytes()).unwrap();
        let value = res.headers_mut().remove(name.clone()).unwrap();
        trailers.append(name, value);
    }
    if !res.headers().contains_key(DATE) {
        let date = date.with_date(HeaderValue::from_bytes).unwrap();
        res.headers_mut().insert(DATE, date);
    }
    // Connection: close 由 writer loop 处理（通过 graceful_shutdown 信号）
    res.headers_mut().remove(CONNECTION);

    if is_eof {
        // 无 body：end_stream=true，writer loop 用 send_response(true) 一帧完成
        let (stream_tx, _) = oneshot::channel();
        let _ = write_tx.send(WriteCmd::Headers {
            respond, res,
            end_stream: true,
            inline_body: None,
            trailers,
            stream_tx,
        });
        return;
    }

    // 3. 小文件快路：收集 body，打包成 inline_body 随 HEADERS 一起发
    //    HEADERS + DATA 在 writer loop 里连续处理，等价于合并帧
    if let BodySize::Sized(n) = body_size {
        if n as usize <= SMALL_BODY_INLINE {
            let mut body = pin!(body);
            let mut buf = crate::bytes::BytesMut::with_capacity(n as usize);
            let mut body_err = false;
            while let Some(r) = poll_fn(|cx| body.as_mut().poll_next(cx)).await {
                match r {
                    Ok(c) => buf.extend_from_slice(&c),
                    Err(_) => { body_err = true; break; }
                }
            }
            let inline = if body_err || buf.is_empty() { None } else { Some(buf.freeze()) };
            let (stream_tx, _) = oneshot::channel();
            let _ = write_tx.send(WriteCmd::Headers {
                respond, res,
                end_stream: false,
                inline_body: inline,
                trailers,
                stream_tx,
            });
            return;
        }
    }

    // 4. 大文件：通过 oneshot 接收 writer loop 返回的 SendStream，然后流式发 DATA
    // trailers 保留在 handler 里，writer loop 对大文件只负责发 HEADERS + 返回 SendStream
    let (stream_tx, stream_rx) = oneshot::channel::<::h2::SendStream<Bytes>>();
    if write_tx.send(WriteCmd::Headers {
        respond, res,
        end_stream: false,
        inline_body: None,
        trailers: HeaderMap::with_capacity(0), // 大文件 trailers 由 handler 自己发
        stream_tx,
    }).is_err() {
        return; // 连接已关闭
    }

    // 等待 writer loop 发完 HEADERS，返回 SendStream
    let mut stream = match stream_rx.await {
        Ok(s) => s,
        Err(_) => return, // writer loop 已退出（连接关闭）
    };

    // 5. 大文件流式发 DATA：capacity() 先判断，有窗口直接发，无窗口才 await
    let mut body = pin!(body);
    loop {
        let chunk = match poll_fn(|cx| body.as_mut().poll_next(cx)).await {
            Some(Ok(c)) => c,
            Some(Err(_)) => return,
            None => break,
        };
        if chunk.is_empty() { continue; }

        let mut offset = 0usize;
        let chunk_len = chunk.len();
        while offset < chunk_len {
            let cap = stream.capacity();
            if cap > 0 {
                let n = cmp::min(cap, chunk_len - offset);
                let bytes = chunk.slice(offset..offset + n);
                if stream.send_data(bytes, false).is_err() { return; }
                offset += n;
                continue;
            }
            stream.reserve_capacity(cmp::min(chunk_len - offset, CHUNK_SIZE));
            match poll_fn(|cx| stream.poll_capacity(cx)).await {
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return,
            }
        }
    }
    // body 全部发完，发 trailers 结束 stream（END_STREAM）
    let _ = stream.send_trailers(trailers);
}

// 小文件内联阈值：≤ 64KB，HEADERS + DATA 在 writer loop 里连续处理
const SMALL_BODY_INLINE: usize = 64 * 1024;

// 大文件 chunk 大小：256KB
const CHUNK_SIZE: usize = 256 * 1024;

// 单连接最多同时在途的 handler 数量：超过则 RST_STREAM，客户端会重试
const MAX_PENDING_PER_CONN: usize = 256;
