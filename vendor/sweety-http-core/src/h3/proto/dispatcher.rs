use core::{
    fmt,
    future::poll_fn,
    marker::PhantomData,
    net::SocketAddr,
    pin::pin,
};

use ::h3::server::{self, RequestStream};
use futures_core::stream::Stream;
use h3_quinn::{BidiStream as QuinnBidiStream, RecvStream as QuinnRecvStream};
use sweety_io_compat::net::QuicStream;
use sweety_service::Service;
use sweety_collection::futures::{Select, SelectOutput};

use crate::{
    bytes::Bytes,
    error::HttpServiceError,
    h3::{body::RequestBody, error::Error},
    http::{Extension, Request, RequestExt, Response},
    util::futures::Queue,
};

/// Http/3 dispatcher
pub(crate) struct Dispatcher<'a, S, ReqB> {
    io: QuicStream,
    addr: SocketAddr,
    service: &'a S,
    _req_body: PhantomData<ReqB>,
}

impl<'a, S, ReqB, ResB, BE> Dispatcher<'a, S, ReqB>
where
    S: Service<Request<RequestExt<ReqB>>, Response = Response<ResB>>,
    S::Error: fmt::Debug,
    ResB: Stream<Item = Result<Bytes, BE>>,
    BE: fmt::Debug,
    ReqB: From<RequestBody>,
{
    pub(crate) fn new(io: QuicStream, addr: SocketAddr, service: &'a S) -> Self {
        Self {
            io,
            addr,
            service,
            _req_body: PhantomData,
        }
    }

    pub(crate) async fn run(self) -> Result<(), Error<S::Error, BE>> {
        let conn = self.io.connecting().await?;
        let conn = h3_quinn::Connection::new(conn);
        let mut builder = server::builder();
        // 防 header 爆炸攻击，单请求头总大小限 64KB
        builder.max_field_section_size(65536);
        let mut conn = builder.build(conn).await?;

        let mut queue = Queue::new();
        // 单连接并发流上限（等价 H2 max_concurrent_streams）
        const MAX_CONCURRENT: usize = 256;

        loop {
            if queue.len() >= MAX_CONCURRENT {
                match queue.next2().await {
                    Err(e) => HttpServiceError::from(e).log("h3_handler"),
                    Ok(()) => {}
                }
                continue;
            }

            match conn.accept().select(queue.next()).await {
                SelectOutput::A(Ok(Some((req, stream)))) => {
                    let addr = self.addr;
                    // 整个 BidiStream 传给 handler，在 handler 内 split
                    // 这样 rx/tx 都在 h3_handler 栈帧上，finish() 后才一起 drop
                    queue.push(async move {
                        h3_handler(self.service, req, stream, addr).await
                    });
                }
                SelectOutput::A(Ok(None)) => break,
                SelectOutput::A(Err(e)) => {
                    match e.get_error_level() {
                        ::h3::error::ErrorLevel::StreamError => continue,
                        ::h3::error::ErrorLevel::ConnectionError => break,
                    }
                }
                SelectOutput::B(Err(e)) => HttpServiceError::from(e).log("h3_handler"),
                SelectOutput::B(Ok(())) => {}
            }
        }

        queue.drain().await;
        Ok(())
    }
}

/// 大文件流水线阈值：超过此大小改用流水线发送，避免整块 collect 内存爆发
/// 与 H2 SMALL_BODY_INLINE 对齐（1MB）
const H3_INLINE_MAX: usize = 1024 * 1024;

/// H3 请求处理核心
///
/// 关键设计：
/// - BidiStream 在 handler 内 split，tx 发响应，rx 用 ManuallyDrop 包装后传给 service
/// - RequestBody drop 时不发 STOP_SENDING（ManuallyDrop 阻止 RecvStream::drop）
/// - finish() 完成后函数返回，quinn 连接关闭时统一回收流资源
/// - 小文件（≤1MB）：同步 poll 后整块 send_data，对标 H2 快路径
/// - 大文件（>1MB）：流水线 send_data，不 collect 全部内存
async fn h3_handler<S, ReqB, ResB, BE>(
    service: &S,
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

    // 在 handler 内 split：tx/rx 都在本函数栈帧
    // rx 用 ManuallyDrop 包装，drop 时不发 STOP_SENDING
    // quinn 连接关闭时统一回收流资源
    let (mut tx, rx): (_, RequestStream<QuinnRecvStream, Bytes>) = stream.split();
    let body = RequestBody::new(rx);
    let http_req = req.map(|_| {
        RequestExt::from_parts(
            ReqB::from(body),
            Extension::new(addr, true),
        )
    });
    let resp = service.call(http_req).await.map_err(Error::Service)?;
    let (parts, res_body) = resp.into_parts();

    let body_size = BodySize::from_stream(&res_body);

    // 注入 Content-Length
    let mut parts = parts;
    if let BodySize::Sized(n) = body_size {
        use crate::http::header::{CONTENT_LENGTH, HeaderValue};
        if !parts.headers.contains_key(CONTENT_LENGTH) {
            let mut ibuf = itoa::Buffer::new();
            if let Ok(v) = HeaderValue::from_str(ibuf.format(n)) {
                parts.headers.insert(CONTENT_LENGTH, v);
            }
        }
    }

    let res = crate::http::Response::from_parts(parts, ());
    tx.send_response(res).await?;

    let mut body = pin!(res_body);

    match body_size {
        BodySize::None => {
            tx.finish().await?;
        }
        BodySize::Sized(n) if n <= H3_INLINE_MAX => {
            // 同步 poll 尝试（mmap/cache 命中时单块立即 Ready，零 await）
            let waker = futures_util::task::noop_waker_ref();
            let mut cx = core::task::Context::from_waker(waker);
            let data = match body.as_mut().poll_next(&mut cx) {
                core::task::Poll::Ready(None) => None,
                core::task::Poll::Ready(Some(Ok(first))) => {
                    match body.as_mut().poll_next(&mut cx) {
                        core::task::Poll::Ready(None) => Some(first),
                        core::task::Poll::Ready(Some(Ok(second))) => {
                            let mut buf = crate::bytes::BytesMut::with_capacity(n);
                            buf.extend_from_slice(&first);
                            buf.extend_from_slice(&second);
                            loop {
                                match body.as_mut().poll_next(&mut cx) {
                                    core::task::Poll::Ready(Some(Ok(c))) => buf.extend_from_slice(&c),
                                    core::task::Poll::Ready(_) => break,
                                    core::task::Poll::Pending => {
                                        while let Some(Ok(c)) = poll_fn(|cx| body.as_mut().poll_next(cx)).await {
                                            buf.extend_from_slice(&c);
                                        }
                                        break;
                                    }
                                }
                            }
                            Some(buf.freeze())
                        }
                        core::task::Poll::Pending => {
                            let mut buf = crate::bytes::BytesMut::with_capacity(n);
                            buf.extend_from_slice(&first);
                            while let Some(Ok(c)) = poll_fn(|cx| body.as_mut().poll_next(cx)).await {
                                buf.extend_from_slice(&c);
                            }
                            Some(buf.freeze())
                        }
                        core::task::Poll::Ready(Some(Err(_))) => Some(first),
                    }
                }
                core::task::Poll::Ready(Some(Err(_))) => None,
                core::task::Poll::Pending => {
                    let mut buf = crate::bytes::BytesMut::with_capacity(n);
                    while let Some(Ok(c)) = poll_fn(|cx| body.as_mut().poll_next(cx)).await {
                        buf.extend_from_slice(&c);
                    }
                    Some(buf.freeze())
                }
            };
            if let Some(data) = data {
                tx.send_data(data).await?;
            }
            tx.finish().await?;
        }
        _ => {
            while let Some(res) = poll_fn(|cx| body.as_mut().poll_next(cx)).await {
                let bytes = res.map_err(Error::Body)?;
                if !bytes.is_empty() {
                    tx.send_data(bytes).await?;
                }
            }
            tx.finish().await?;
        }
    }

    Ok(())
}

