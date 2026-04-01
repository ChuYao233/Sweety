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
use std::{collections::VecDeque, rc::Rc, sync::Arc};

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

/// handler task → writer loop 的写命令
enum WriteCmd {
    Headers {
        respond:     SendResponse<Bytes>,
        res:         Response<()>,
        end_stream:  bool,
        inline_body: Option<Bytes>,
        trailers:    HeaderMap,
    },
    HeadersStream {
        respond:  SendResponse<Bytes>,
        res:      Response<()>,
        body_rx:  mpsc::Receiver<DataChunk>,
        done_tx:  oneshot::Sender<()>,
    },
}

struct DataChunk {
    data:       Bytes,
    end_stream: bool,
    trailers:   HeaderMap,
}

struct StreamState {
    stream:  ::h2::SendStream<Bytes>,
    body_rx: mpsc::Receiver<DataChunk>,
    done_tx: oneshot::Sender<()>,
    pending: Option<DataChunk>,
}

async fn conn_writer_loop(mut rx: mpsc::UnboundedReceiver<WriteCmd>) {
    let mut streams: VecDeque<StreamState> = VecDeque::new();

    loop {
        loop {
            match rx.try_recv() {
                Ok(cmd) => handle_cmd(cmd, &mut streams),
                Err(_) => break,
            }
        }

        let mut state = match streams.pop_front() {
            Some(s) => s,
            None => {
                match rx.recv().await {
                    Some(cmd) => { handle_cmd(cmd, &mut streams); continue; }
                    None => break,
                }
            }
        };

        if state.pending.is_none() {
            tokio::select! {
                biased;
                cmd_opt = rx.recv() => {
                    match cmd_opt {
                        Some(cmd) => handle_cmd(cmd, &mut streams),
                        None => break,
                    }
                    streams.push_front(state);
                    continue;
                }
                chunk_opt = state.body_rx.recv() => {
                    match chunk_opt {
                        Some(chunk) => state.pending = Some(chunk),
                        None => { let _ = state.done_tx.send(()); continue; }
                    }
                }
            }
        }

        let chunk = state.pending.as_ref().unwrap();

        if state.stream.capacity() == 0 {
            state.stream.reserve_capacity(CHUNK_SIZE);
            tokio::select! {
                biased;
                cmd_opt = rx.recv() => {
                    match cmd_opt {
                        Some(cmd) => handle_cmd(cmd, &mut streams),
                        None => break,
                    }
                    streams.push_back(state);
                    continue;
                }
                cap_opt = poll_fn(|cx| state.stream.poll_capacity(cx)) => {
                    match cap_opt {
                        Some(Ok(cap)) if cap > 0 => {}
                        _ => { let _ = state.done_tx.send(()); continue; }
                    }
                }
            }
        }

        let cap = state.stream.capacity();
        let data_len = chunk.data.len();
        let send_len = cmp::min(cap, cmp::min(data_len, CHUNK_SIZE));
        let is_chunk_done = send_len >= data_len;

        if is_chunk_done && chunk.end_stream {
            let data     = chunk.data.clone();
            let trailers = chunk.trailers.clone();
            let has_trailers = !trailers.is_empty();
            state.pending = None;
            if !data.is_empty() { let _ = state.stream.send_data(data, !has_trailers); }
            if has_trailers { let _ = state.stream.send_trailers(trailers); }
            else { let _ = state.stream.send_data(Bytes::new(), true); }
            let _ = state.done_tx.send(());
        } else if is_chunk_done {
            let data = chunk.data.clone();
            state.pending = None;
            let _ = state.stream.send_data(data, false);
            streams.push_back(state);
        } else {
            let data      = chunk.data.slice(..send_len);
            let remaining = chunk.data.slice(send_len..);
            let end_stream = chunk.end_stream;
            let trailers   = chunk.trailers.clone();
            state.pending  = Some(DataChunk { data: remaining, end_stream, trailers });
            let _ = state.stream.send_data(data, false);
            streams.push_back(state);
        }
    }
}

fn handle_cmd(cmd: WriteCmd, streams: &mut VecDeque<StreamState>) {
    match cmd {
        WriteCmd::Headers { mut respond, res, end_stream, inline_body, trailers } => {
            if end_stream {
                let _ = respond.send_response(res, true);
                return;
            }
            let mut stream = match respond.send_response(res, false) {
                Ok(s) => s,
                Err(_) => return,
            };
            if let Some(bytes) = inline_body {
                let has_trailers = !trailers.is_empty();
                let len = bytes.len();
                if len > 0 {
                    stream.reserve_capacity(len);
                    let cap = stream.capacity();
                    if cap >= len {
                        let _ = stream.send_data(bytes, !has_trailers);
                        if has_trailers { let _ = stream.send_trailers(trailers); }
                    } else {
                        let sent = if cap > 0 { let _ = stream.send_data(bytes.slice(..cap), false); cap } else { 0 };
                        let (dummy_tx, body_rx) = mpsc::channel::<DataChunk>(1);
                        let (done_tx, _) = oneshot::channel::<()>();
                        drop(dummy_tx);
                        streams.push_back(StreamState {
                            stream, body_rx, done_tx,
                            pending: Some(DataChunk { data: bytes.slice(sent..), end_stream: true, trailers }),
                        });
                    }
                } else {
                    if has_trailers { let _ = stream.send_trailers(trailers); }
                    else { let _ = stream.send_data(Bytes::new(), true); }
                }
            } else {
                let _ = stream.send_data(Bytes::new(), true);
            }
        }
        WriteCmd::HeadersStream { mut respond, res, body_rx, done_tx } => {
            let stream = match respond.send_response(res, false) {
                Ok(s) => s,
                Err(_) => { let _ = done_tx.send(()); return; }
            };
            streams.push_back(StreamState { stream, body_rx, done_tx, pending: None });
        }
    }
}

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
    ResB: Stream<Item = Result<Bytes, BE>> + 'static,
    BE: fmt::Debug + 'static,
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

        let (write_tx, write_rx) = mpsc::unbounded_channel::<WriteCmd>();
        let pending_count = Rc::new(std::cell::Cell::new(0usize));
        let local = LocalSet::new();

        local.spawn_local(conn_writer_loop(write_rx));

        let accept_result: Result<(), Error<S::Error, BE>> = local.run_until(async {
            loop {
                match io.accept().select(&mut ping_pong).await {
                    SelectOutput::A(Some(Ok((req, respond)))) => {
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
                        if max_pending > 0 && pc.get() >= max_pending {
                            io.graceful_shutdown();
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
    let resp = match fut.await {
        Ok(r) => r,
        Err(e) => { trace!("h2 service error: {:?}", e); return; }
    };

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

    let mut trailers = HeaderMap::with_capacity(0);
    while let Some(value) = res.headers_mut().remove(TRAILER) {
        let name = HeaderName::from_bytes(value.as_bytes()).unwrap();
        let value = match res.headers_mut().remove(name.clone()) {
            Some(v) => v,
            None => continue,
        };
        trailers.append(name, value);
    }
    if !res.headers().contains_key(DATE) {
        let date = date.with_date(HeaderValue::from_bytes).unwrap();
        res.headers_mut().insert(DATE, date);
    }
    res.headers_mut().remove(CONNECTION);

    if is_eof {
        let _ = write_tx.send(WriteCmd::Headers { respond, res, end_stream: true, inline_body: None, trailers });
        return;
    }

    // 小文件快路：收集 body 后随 HEADERS 一起发
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
            let _ = write_tx.send(WriteCmd::Headers { respond, res, end_stream: false, inline_body: inline, trailers });
            return;
        }
    }

    // 大文件/流式路径
    let (body_tx, body_rx) = mpsc::channel::<DataChunk>(BODY_CHANNEL_CAP);
    let (done_tx, done_rx) = oneshot::channel::<()>();
    if write_tx.send(WriteCmd::HeadersStream { respond, res, body_rx, done_tx }).is_err() {
        return;
    }

    let mut body = pin!(body);
    'outer: loop {
        let chunk = match poll_fn(|cx| body.as_mut().poll_next(cx)).await {
            Some(Ok(c)) if !c.is_empty() => c,
            Some(Ok(_)) => continue,
            Some(Err(_)) | None => break,
        };
        let mut offset = 0;
        while offset < chunk.len() {
            let end = cmp::min(offset + CHUNK_SIZE, chunk.len());
            let mut dc = DataChunk {
                data: chunk.slice(offset..end),
                end_stream: false,
                trailers: HeaderMap::with_capacity(0),
            };
            offset = end;
            // try_send + yield_now：避免 body_rx 消费者（conn_writer_loop）未被调度时死锁
            loop {
                match body_tx.try_send(dc) {
                    Ok(()) => break,
                    Err(tokio::sync::mpsc::error::TrySendError::Full(back)) => {
                        dc = back;
                        tokio::task::yield_now().await;
                    }
                    Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break 'outer,
                }
            }
        }
    }
    // 发 end_stream
    loop {
        let eos = DataChunk { data: Bytes::new(), end_stream: true, trailers: trailers.clone() };
        match body_tx.try_send(eos) {
            Ok(()) => break,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => tokio::task::yield_now().await,
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
        }
    }
    let _ = done_rx.await;
}

const SMALL_BODY_INLINE: usize = 64 * 1024;
const CHUNK_SIZE: usize = 16 * 1024;
const BODY_CHANNEL_CAP: usize = 4;
