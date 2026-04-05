use core::{
    fmt,
    future::poll_fn,
    marker::PhantomData,
    net::SocketAddr,
    pin::pin,
    task::{Context, Poll},
};

use ::h3::server::{self, RequestResolver, RequestStream};
use futures_core::stream::Stream;
use h3_quinn::RecvStream as QuinnRecvStream;
use std::rc::Rc;
use std::sync::Arc;
use sweety_io_compat::net::QuicStream;
use sweety_service::Service;
use tracing::{debug, warn};

use crate::{
    bytes::{Bytes, BytesMut},
    date::{DateTime, DateTimeHandle},
    error::HttpServiceError,
    h3::{body::RequestBody, error::Error},
    http::{Extension, Request, RequestExt, Response},
};

/// Http/3 dispatcher
pub(crate) struct Dispatcher<S, ReqB> {
    io: QuicStream,
    addr: SocketAddr,
    service: Arc<S>,
    date: Rc<DateTimeHandle>,
    _req_body: PhantomData<ReqB>,
}

impl<S, ReqB, ResB, BE> Dispatcher<S, ReqB>
where
    S: Service<Request<RequestExt<ReqB>>, Response = Response<ResB>> + 'static,
    S::Error: fmt::Debug,
    ResB: Stream<Item = Result<Bytes, BE>>,
    BE: fmt::Debug,
    ReqB: From<RequestBody> + 'static,
{
    pub(crate) fn new(io: QuicStream, addr: SocketAddr, service: Arc<S>, date: Rc<DateTimeHandle>) -> Self {
        Self { io, addr, service, date, _req_body: PhantomData }
    }

    pub(crate) async fn run(self) -> Result<(), Error<S::Error, BE>> {
        let conn = self.io.connecting().await?;
        let conn = h3_quinn::Connection::new(conn);
        let mut builder = server::builder();
        builder.max_field_section_size(65536);
        let mut conn = builder.build(conn).await?;

        use futures_util::{FutureExt as _, stream::FuturesUnordered};
        use futures_util::StreamExt as _;

        debug!(addr = %self.addr, "h3 连接建立");

        // 连接级事件循环：accept 和 handler 并发推进
        let mut handlers = FuturesUnordered::new();
        let mut request_count: u64 = 0;
        const MAX_CONCURRENT: usize = 1024;
        const READY_DRAIN_BUDGET: usize = 32;

        loop {
            // 小批量清理已完成 handler，降低队列堆积导致的抖动
            for _ in 0..READY_DRAIN_BUDGET {
                match handlers.next().now_or_never() {
                    Some(Some(())) => {}
                    _ => break,
                }
            }

            // 背压：满队列时先消费一个完成事件，避免 accept 后丢流
            if handlers.len() >= MAX_CONCURRENT {
                warn!(addr = %self.addr, pending = handlers.len(), "h3 背压：等待 handler 完成");
                if handlers.next().await.is_none() {
                    break;
                }
                continue;
            }

            tokio::select! {
                accept = conn.accept() => {
                    match accept {
                        Ok(Some(resolver)) => {
                            request_count += 1;
                            // 每 1000 请求输出一次连接级摘要
                            if request_count % 1000 == 0 {
                                debug!(
                                    addr = %self.addr,
                                    request_count,
                                    pending_handlers = handlers.len(),
                                    "h3 连接摘要"
                                );
                            }
                            // 低并发快路：没有待处理 handler 时直接 inline，减少一次 future 入队/调度
                            if handlers.is_empty() {
                                if let Err(e) = resolve_and_handle(Arc::clone(&self.service), resolver, self.addr, Rc::clone(&self.date)).await {
                                    HttpServiceError::from(e).log("h3_handler");
                                }
                            } else {
                                handlers.push(run_resolve_and_handle(Arc::clone(&self.service), resolver, self.addr, Rc::clone(&self.date)));
                            }
                        }
                        Ok(None) => {
                            debug!(addr = %self.addr, request_count, "h3 连接正常关闭 (None)");
                            break;
                        }
                        Err(e) => {
                            debug!(addr = %self.addr, request_count, error = %e, "h3 连接错误关闭");
                            break;
                        }
                    }
                }
                Some(_) = handlers.next(), if !handlers.is_empty() => {}
            }
        }

        let remaining = handlers.len();
        if remaining > 0 {
            debug!(addr = %self.addr, remaining, "h3 等待剩余 handler 完成");
        }
        while handlers.next().await.is_some() {}
        debug!(addr = %self.addr, request_count, "h3 连接彻底关闭");
        Ok(())
    }
}

async fn run_resolve_and_handle<S, ReqB, ResB, BE>(
    service: Arc<S>,
    resolver: RequestResolver<h3_quinn::Connection, Bytes>,
    addr: SocketAddr,
    date: Rc<DateTimeHandle>,
)
where
    S: Service<Request<RequestExt<ReqB>>, Response = Response<ResB>>,
    S::Error: fmt::Debug,
    ReqB: From<RequestBody>,
    ResB: Stream<Item = Result<Bytes, BE>>,
    BE: fmt::Debug,
{
    if let Err(e) = resolve_and_handle(service, resolver, addr, date).await {
        HttpServiceError::from(e).log("h3_handler");
    }
}

/// H3 请求处理核心：先 resolve 请求（h3 0.0.8 避免队头阻塞），再直接 chunk 发送
async fn resolve_and_handle<S, ReqB, ResB, BE>(
    service: Arc<S>,
    resolver: RequestResolver<h3_quinn::Connection, Bytes>,
    addr: SocketAddr,
    date: Rc<DateTimeHandle>,
) -> Result<(), Error<S::Error, BE>>
where
    S: Service<Request<RequestExt<ReqB>>, Response = Response<ResB>>,
    S::Error: fmt::Debug,
    ReqB: From<RequestBody>,
    ResB: Stream<Item = Result<Bytes, BE>>,
    BE: fmt::Debug,
{
    use crate::body::BodySize;

    let (req, stream) = resolver.resolve_request().await?;
    let (mut tx, rx): (_, RequestStream<QuinnRecvStream, Bytes>) = stream.split();
    let body = RequestBody::new(rx);

    let http_req = req.map(|_| {
        RequestExt::from_parts(ReqB::from(body), Extension::new(addr, true))
    });
    let resp = service.call(http_req).await.map_err(Error::Service)?;
    let (mut parts, res_body) = resp.into_parts();

    let body_size = BodySize::from_stream(&res_body);

    // 注入 Content-Length
    if let BodySize::Sized(n) = body_size {
        use crate::http::header::{CONTENT_LENGTH, HeaderValue};
        if !parts.headers.contains_key(CONTENT_LENGTH) {
            let mut ibuf = itoa::Buffer::new();
            if let Ok(v) = HeaderValue::from_str(ibuf.format(n)) {
                parts.headers.insert(CONTENT_LENGTH, v);
            }
        }
    }

    // RFC 7231 §7.1.1.2 MUST：注入 Date 头（与 H2 dispatcher 行为一致）
    {
        use crate::http::header::{DATE, HeaderValue};
        if !parts.headers.contains_key(DATE) {
            if let Ok(v) = date.with_date(HeaderValue::from_bytes) {
                parts.headers.insert(DATE, v);
            }
        }
    }

    tx.send_response(crate::http::Response::from_parts(parts, ())).await?;

    // 小 body 内联快路：同步聚合后单次 send_data，减少多次 await 往返
    const INLINE_SEND_MAX: usize = 32 * 1024;
    if let BodySize::Sized(n) = body_size {
        if (n as usize) <= INLINE_SEND_MAX {
            let mut body = pin!(res_body);
            let waker = futures_util::task::noop_waker();
            let mut cx = Context::from_waker(&waker);
            let mut buf = BytesMut::with_capacity(n as usize);

            loop {
                match body.as_mut().poll_next(&mut cx) {
                    Poll::Ready(Some(Ok(bytes))) => buf.extend_from_slice(&bytes),
                    Poll::Ready(Some(Err(e))) => return Err(Error::Body(e)),
                    Poll::Ready(None) => {
                        if !buf.is_empty() {
                            tx.send_data(buf.freeze()).await?;
                        }
                        // fire-and-forget：不等客户端 ACK FIN（~26ms delayed ACK），立即返回
                        tokio::task::spawn_local(async move { let _ = tx.finish().await; });
                        return Ok(());
                    }
                    Poll::Pending => break,
                }
            }

            // 仍有后续异步数据：先发送已聚合部分，再回退到流式发送
            if !buf.is_empty() {
                tx.send_data(buf.freeze()).await?;
            }

            while let Some(res) = poll_fn(|cx| body.as_mut().poll_next(cx)).await {
                let bytes = res.map_err(Error::Body)?;
                if !bytes.is_empty() {
                    tx.send_data(bytes).await?;
                }
            }
            // fire-and-forget：不等客户端 ACK FIN
            tokio::task::spawn_local(async move { let _ = tx.finish().await; });
            return Ok(());
        }
    }

    // 大文件：每次 await 一个 chunk，发送后继续下一块
    // spawn_blocking 的线程池调度间隙自然让 quinn 消费发送缓冲
    // send_data await 本身在 quinn send_window 耗尽时会阻塞，形成真正背压
    let mut body = pin!(res_body);
    while let Some(res) = poll_fn(|cx| body.as_mut().poll_next(cx)).await {
        let bytes = res.map_err(Error::Body)?;
        if !bytes.is_empty() {
            tx.send_data(bytes).await?;
        }
    }
    // fire-and-forget：不等客户端 ACK FIN
    tokio::task::spawn_local(async move { let _ = tx.finish().await; });
    Ok(())
}
