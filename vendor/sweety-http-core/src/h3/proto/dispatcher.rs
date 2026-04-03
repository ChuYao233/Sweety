use core::{
    fmt,
    future::poll_fn,
    marker::PhantomData,
    net::SocketAddr,
    pin::pin,
    task::{Context, Poll},
};

use ::h3::server::{self, RequestStream};
use futures_core::stream::Stream;
use h3_quinn::{BidiStream as QuinnBidiStream, RecvStream as QuinnRecvStream};
use std::sync::Arc;
use sweety_io_compat::net::QuicStream;
use sweety_service::Service;

use crate::{
    bytes::{Bytes, BytesMut},
    error::HttpServiceError,
    h3::{body::RequestBody, error::Error},
    http::{Extension, Request, RequestExt, Response},
};

/// Http/3 dispatcher
pub(crate) struct Dispatcher<S, ReqB> {
    io: QuicStream,
    addr: SocketAddr,
    service: Arc<S>,
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
    pub(crate) fn new(io: QuicStream, addr: SocketAddr, service: Arc<S>) -> Self {
        Self { io, addr, service, _req_body: PhantomData }
    }

    pub(crate) async fn run(self) -> Result<(), Error<S::Error, BE>> {
        let conn = self.io.connecting().await?;
        let conn = h3_quinn::Connection::new(conn);
        let mut builder = server::builder();
        builder.max_field_section_size(65536);
        let mut conn = builder.build(conn).await?;

        use futures_util::{FutureExt as _, stream::FuturesUnordered};
        use futures_util::StreamExt as _;

        // 连接级事件循环：accept 和 handler 并发推进
        let mut handlers = FuturesUnordered::new();
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
                if handlers.next().await.is_none() {
                    break;
                }
                continue;
            }

            tokio::select! {
                accept = conn.accept() => {
                    match accept {
                        Ok(Some((req, stream))) => {
                            // 低并发快路：没有待处理 handler 时直接 inline，减少一次 future 入队/调度
                            if handlers.is_empty() {
                                if let Err(e) = h3_handler(Arc::clone(&self.service), req, stream, self.addr).await {
                                    HttpServiceError::from(e).log("h3_handler");
                                }
                            } else {
                                handlers.push(run_handler(Arc::clone(&self.service), req, stream, self.addr));
                            }
                        }
                        Ok(None) => break,
                        Err(e) => match e.get_error_level() {
                            ::h3::error::ErrorLevel::StreamError => {}
                            ::h3::error::ErrorLevel::ConnectionError => break,
                        },
                    }
                }
                Some(_) = handlers.next(), if !handlers.is_empty() => {}
            }
        }

        while handlers.next().await.is_some() {}
        Ok(())
    }
}

async fn run_handler<S, ReqB, ResB, BE>(
    service: Arc<S>,
    req: Request<()>,
    stream: RequestStream<QuinnBidiStream<Bytes>, Bytes>,
    addr: SocketAddr,
)
where
    S: Service<Request<RequestExt<ReqB>>, Response = Response<ResB>>,
    S::Error: fmt::Debug,
    ReqB: From<RequestBody>,
    ResB: Stream<Item = Result<Bytes, BE>>,
    BE: fmt::Debug,
{
    if let Err(e) = h3_handler(service, req, stream, addr).await {
        HttpServiceError::from(e).log("h3_handler");
    }
}

/// H3 请求处理核心：直接 chunk 发送，不做整块 collect
async fn h3_handler<S, ReqB, ResB, BE>(
    service: Arc<S>,
    req: Request<()>,
    stream: RequestStream<QuinnBidiStream<Bytes>, Bytes>,
    addr: SocketAddr,
) -> Result<(), Error<S::Error, BE>>
where
    S: Service<Request<RequestExt<ReqB>>, Response = Response<ResB>>,
    S::Error: fmt::Debug,
    ReqB: From<RequestBody>,
    ResB: Stream<Item = Result<Bytes, BE>>,
    BE: fmt::Debug,
{
    use crate::body::BodySize;

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

    // H3 body 快路：先同步 poll，只有 Pending 时才 await，减少每 chunk 一次调度往返
    let mut body = pin!(res_body);
    let waker = futures_util::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    loop {
        match body.as_mut().poll_next(&mut cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                if !bytes.is_empty() {
                    tx.send_data(bytes).await?;
                }
            }
            Poll::Ready(Some(Err(e))) => return Err(Error::Body(e)),
            Poll::Ready(None) => break,
            Poll::Pending => {
                match poll_fn(|cx| body.as_mut().poll_next(cx)).await {
                    Some(Ok(bytes)) => {
                        if !bytes.is_empty() {
                            tx.send_data(bytes).await?;
                        }
                    }
                    Some(Err(e)) => return Err(Error::Body(e)),
                    None => break,
                }
            }
        }
    }
    // fire-and-forget：不等客户端 ACK FIN
    tokio::task::spawn_local(async move { let _ = tx.finish().await; });
    Ok(())
}
