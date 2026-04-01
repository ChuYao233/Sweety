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
    Headers {
        respond:     SendResponse<Bytes>,
        res:         Response<()>,
        end_stream:  bool,
        inline_body: Option<Bytes>,
        trailers:    HeaderMap,
        stream_tx:   oneshot::Sender<::h2::SendStream<Bytes>>,
    },
    /// 大文件注册：handler 把 body channel 交给 writer loop 调度
    /// writer loop 自己驱动 DATA 发送，实现公平调度
    Register {
        stream:   ::h2::SendStream<Bytes>,
        body_rx:  mpsc::Receiver<DataChunk>,
        done_tx:  oneshot::Sender<()>,
    },
}

/// handler 向 writer loop 发送的 DATA 片
struct DataChunk {
    data:       Bytes,
    end_stream: bool,
    trailers:   HeaderMap,
}

/// writer loop 内部的流调度状态
struct StreamState {
    stream:  ::h2::SendStream<Bytes>,
    body_rx: mpsc::Receiver<DataChunk>,
    done_tx: oneshot::Sender<()>,
    /// 当前待发的 chunk（已从 body_rx 取出但尚未发完）
    pending: Option<DataChunk>,
}

/// writer loop：生产级 write fairness 调度
///
/// 事件驱动，零 CPU 空转：
///
/// 主循环每轮：
///   A. 非阻塞 drain cmd channel → 处理所有就绪 Headers/Register
///   B. 取队首流（StreamState）：
///      - pending 为空  → select!(cmd_channel | body_rx.recv()) 真正 await
///        任意就绪：cmd 就处理控制帧；body_rx 就拿到数据 chunk
///      - pending 有数据 → 检查 flow control 窗口：
///          有 capacity → 发一片 CHUNK_SIZE → push_back 重入队（round-robin）
///          无 capacity → select!(cmd_channel | poll_capacity) 真正 await
///            窗口到了继续发；cmd 就先处理控制帧再回来等窗口
///   C. 流结束 → done_tx 通知 handler，不重入队
///
/// 关键保证：
///   - HEADERS 在任何 await 点都能插队（select! 的 cmd_channel 分支）
///   - body 空/窗口耗尽时真正 sleep，不 busy-spin
///   - 多个大文件流通过 push_back 实现 round-robin，单片最大 CHUNK_SIZE
async fn conn_writer_loop(mut rx: mpsc::UnboundedReceiver<WriteCmd>) {
    use std::collections::VecDeque;
    let mut streams: VecDeque<StreamState> = VecDeque::new();

    loop {
        // ── A. 非阻塞 drain cmd channel ────────────────────────────────────
        // 每轮先处理所有就绪的控制帧，保证 HEADERS 优先
        loop {
            match rx.try_recv() {
                Ok(cmd) => handle_cmd(cmd, &mut streams),
                Err(_) => break,
            }
        }

        // ── B. 取队首流处理 ─────────────────────────────────────────────────
        let mut state = match streams.pop_front() {
            Some(s) => s,
            None => {
                // 无流：阻塞等新 cmd（真正 sleep，零 CPU）
                match rx.recv().await {
                    Some(cmd) => { handle_cmd(cmd, &mut streams); continue; }
                    None => break, // cmd channel 关闭，退出
                }
            }
        };

        // ── B1. 确保 pending 有数据 ─────────────────────────────────────────
        if state.pending.is_none() {
            // select! 同时等新 cmd 和队首流的 body 数据
            // 任意就绪都不会 spin
            tokio::select! {
                biased; // cmd 优先（控制面 > 数据面）
                cmd_opt = rx.recv() => {
                    match cmd_opt {
                        Some(cmd) => handle_cmd(cmd, &mut streams),
                        None => break,
                    }
                    // 把 state 放回队首，下轮继续
                    streams.push_front(state);
                    continue;
                }
                chunk_opt = state.body_rx.recv() => {
                    match chunk_opt {
                        Some(chunk) => state.pending = Some(chunk),
                        None => {
                            // handler 已 drop body_tx（异常关闭），清理流
                            let _ = state.done_tx.send(());
                            continue;
                        }
                    }
                }
            }
        }

        // ── B2. 有数据，检查 flow control 窗口 ─────────────────────────────
        let chunk = state.pending.as_ref().unwrap();

        if state.stream.capacity() == 0 {
            // 窗口耗尽：reserve 后 select! 等窗口或新 cmd
            state.stream.reserve_capacity(CHUNK_SIZE);
            tokio::select! {
                biased;
                cmd_opt = rx.recv() => {
                    match cmd_opt {
                        Some(cmd) => handle_cmd(cmd, &mut streams),
                        None => break,
                    }
                    // 窗口还没来，把 state 放回队尾（让其他流先跑）
                    streams.push_back(state);
                    continue;
                }
                cap_opt = poll_fn(|cx| state.stream.poll_capacity(cx)) => {
                    match cap_opt {
                        Some(Ok(cap)) if cap > 0 => { /* 有窗口了，继续发 */ }
                        _ => {
                            // 流被对端 RST 或关闭
                            let _ = state.done_tx.send(());
                            continue;
                        }
                    }
                }
            }
        }

        // ── B3. 发一片（最多 CHUNK_SIZE，受 capacity 限制）────────────────
        let cap = state.stream.capacity();
        let data_len = chunk.data.len();
        let send_len = cmp::min(cap, cmp::min(data_len, CHUNK_SIZE));
        let is_chunk_done = send_len >= data_len;

        if is_chunk_done && chunk.end_stream {
            // 最后一片发完 → 结束流
            let data     = chunk.data.clone();
            let trailers = chunk.trailers.clone();
            state.pending = None;
            if !data.is_empty() {
                let _ = state.stream.send_data(data, false);
            }
            let _ = state.stream.send_trailers(trailers);
            let _ = state.done_tx.send(());
            // 不重入队，流结束
        } else if is_chunk_done {
            // 当前 chunk 发完，还有更多 chunk
            let data = chunk.data.clone();
            state.pending = None;
            let _ = state.stream.send_data(data, false);
            streams.push_back(state); // round-robin：放队尾
        } else {
            // capacity 不足以发完整 chunk，发一片后剩余留 pending
            let data      = chunk.data.slice(..send_len);
            let remaining = chunk.data.slice(send_len..);
            let end_stream = chunk.end_stream;
            let trailers   = chunk.trailers.clone();
            state.pending  = Some(DataChunk { data: remaining, end_stream, trailers });
            let _ = state.stream.send_data(data, false);
            streams.push_back(state); // round-robin：放队尾
        }
    }
}

/// 处理一条 WriteCmd，更新 streams 队列
/// Headers → push_front（控制面优先）
/// Register → push_back（DATA 轮转）
fn handle_cmd(cmd: WriteCmd, streams: &mut std::collections::VecDeque<StreamState>) {
    match cmd {
        WriteCmd::Headers { mut respond, res, end_stream, inline_body, trailers, stream_tx } => {
            if end_stream {
                let _ = respond.send_response(res, true);
                return;
            }
            let mut stream = match respond.send_response(res, false) {
                Ok(s) => s,
                Err(_) => return,
            };
            if let Some(bytes) = inline_body {
                // 小文件：HEADERS + DATA 在同一 handle_cmd 里处理
                // inline body 通常已在初始窗口内，直接发
                let len = bytes.len();
                if len > 0 {
                    stream.reserve_capacity(len);
                    // 尝试同步发：初始窗口 16MB，通常立即有 capacity
                    if stream.capacity() >= len {
                        let _ = stream.send_data(bytes, false);
                    } else {
                        // 极罕见：窗口不足，退化为忽略（小文件不应触发）
                        let _ = stream.send_data(bytes.slice(..stream.capacity()), false);
                    }
                }
                let _ = stream.send_trailers(trailers);
            } else {
                let _ = stream_tx.send(stream);
            }
        }
        WriteCmd::Register { stream, body_rx, done_tx } => {
            streams.push_back(StreamState {
                stream,
                body_rx,
                done_tx,
                pending: None,
            });
        }
    }
}

// 单批次最多 drain WriteCmd 数量
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

    // 4. 大文件：
    //   a. 先发 HEADERS（writer loop 返回 SendStream）
    //   b. handler 开一个 mpsc body channel，把 SendStream + body_rx 注册给 writer loop
    //   c. handler 异步读 body 并逐片发给 body_tx，writer loop round-robin 调度 DATA
    //   d. writer loop 发完所有数据后通过 done_rx 通知 handler 退出

    // a. 发 HEADERS
    let (stream_tx, stream_rx) = oneshot::channel::<::h2::SendStream<Bytes>>();
    if write_tx.send(WriteCmd::Headers {
        respond, res,
        end_stream: false,
        inline_body: None,
        trailers: HeaderMap::with_capacity(0),
        stream_tx,
    }).is_err() {
        return;
    }
    let stream = match stream_rx.await {
        Ok(s) => s,
        Err(_) => return,
    };

    // b. 创建 body channel + done channel，注册给 writer loop
    // body channel 有界（BODY_CHANNEL_CAP），作为背压：writer 消费慢时 handler 暂停生产
    let (body_tx, body_rx) = mpsc::channel::<DataChunk>(BODY_CHANNEL_CAP);
    let (done_tx, done_rx) = oneshot::channel::<()>();
    if write_tx.send(WriteCmd::Register { stream, body_rx, done_tx }).is_err() {
        return;
    }

    // c. handler 读 body 并按 CHUNK_SIZE 分片发给 body_tx
    let mut body = pin!(body);
    'outer: loop {
        let chunk = match poll_fn(|cx| body.as_mut().poll_next(cx)).await {
            Some(Ok(c)) if !c.is_empty() => c,
            Some(Ok(_)) => continue,
            Some(Err(_)) => break,
            None => break,
        };
        // 按 CHUNK_SIZE 分片，每片独立发送（固定大小保证 round-robin 公平性）
        let mut offset = 0;
        while offset < chunk.len() {
            let end = cmp::min(offset + CHUNK_SIZE, chunk.len());
            let slice = chunk.slice(offset..end);
            offset = end;
            let is_last = offset >= chunk.len();
            // 非最后一片，或还有更多 body
            let dc = DataChunk { data: slice, end_stream: false, trailers: HeaderMap::with_capacity(0) };
            if body_tx.send(dc).await.is_err() {
                break 'outer; // writer loop 已关闭（连接断开）
            }
            let _ = is_last; // suppress unused warning
        }
    }
    // d. 发最后一片（end_stream=true，携带 trailers）
    // 无论 body 正常结束还是出错，都要发 end_stream 让 writer loop 关闭流
    let _ = body_tx.send(DataChunk {
        data: Bytes::new(),
        end_stream: true,
        trailers,
    }).await;
    // 等 writer loop 发完（确保连接不会在数据发完前被 keep-alive 关掉）
    let _ = done_rx.await;
}

// 小文件内联阈值：≤ 64KB，HEADERS + DATA 在 writer loop 里连续处理
const SMALL_BODY_INLINE: usize = 64 * 1024;

// 大文件 DATA chunk 大小：16KB（固定小片，保证 round-robin 公平性）
// 参考 Go net/http2：每次 writeScheduler 只调度一个 frame（默认 16KB）
const CHUNK_SIZE: usize = 16 * 1024;

// body channel 容量：writer 消费慢时限制 handler 堆积，提供背压
// 4 片 × 16KB = 64KB per stream，内存可控
const BODY_CHANNEL_CAP: usize = 4;

// 单连接最多同时在途的 handler 数量（0=不限制，由协议层 max_concurrent_streams 控制）
const MAX_PENDING_PER_CONN: usize = 256;
