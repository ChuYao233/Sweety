use core::{
    fmt,
    future::{Future, poll_fn},
    marker::PhantomData,
    net::SocketAddr,
    pin::pin,
};

use ::h3::{
    quic::SendStream,
    server::{self, RequestStream},
};
use futures_core::stream::Stream;
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
        // wait for connecting.
        let conn = self.io.connecting().await?;

        // construct h3 connection from quinn connection.
        let conn = h3_quinn::Connection::new(conn);
        let mut builder = server::builder();
        // 防 header 爆炸攻击（等价 H2 SETTINGS_MAX_HEADER_LIST_SIZE），单请求头总大小 64KB
        builder.max_field_section_size(65536);
        let mut conn = builder.build(conn).await?;

        let mut queue = Queue::new();
        // 单连接并发流上限（等价 H2 max_concurrent_streams）
        const MAX_CONCURRENT: usize = 256;

        loop {
            // 超过并发上限时只处理已有流，不 accept 新流，背压由此传到客户端
            if queue.len() >= MAX_CONCURRENT {
                match queue.next2().await {
                    Err(e) => HttpServiceError::from(e).log("h3_handler"),
                    Ok(()) => {}
                }
                continue;
            }

            match conn.accept().select(queue.next()).await {
                SelectOutput::A(Ok(Some((req, stream)))) => {
                    let (tx, rx) = stream.split();
                    let req = req.map(|_| {
                        let body = ReqB::from(RequestBody(rx));
                        RequestExt::from_parts(body, Extension::new(self.addr, true))
                    });
                    queue.push(async move {
                        let fut = self.service.call(req);
                        h3_handler(fut, tx).await
                    });
                }
                SelectOutput::A(Ok(None)) => break,
                SelectOutput::A(Err(e)) => {
                    // h3 官方推荐：StreamError 仅影响当前流，ConnectionError 才断连接
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

async fn h3_handler<'a, Fut, C, ResB, SE, BE>(
    fut: Fut,
    mut stream: RequestStream<C, Bytes>,
) -> Result<(), Error<SE, BE>>
where
    Fut: Future<Output = Result<Response<ResB>, SE>> + 'a,
    C: SendStream<Bytes>,
    ResB: Stream<Item = Result<Bytes, BE>>,
{
    let (parts, body) = fut.await.map_err(Error::Service)?.into_parts();
    let res = Response::from_parts(parts, ());
    stream.send_response(res).await?;

    let mut body = pin!(body);

    while let Some(res) = poll_fn(|cx| body.as_mut().poll_next(cx)).await {
        let bytes = res.map_err(Error::Body)?;
        stream.send_data(bytes).await?;
    }

    stream.finish().await?;

    Ok(())
}
