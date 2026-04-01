use core::{fmt, net::SocketAddr, pin::pin};

use futures_core::Stream;
use sweety_io_compat::io::{AsyncIo, PollIoAdapter};
use sweety_service::Service;

use crate::{
    bytes::Bytes,
    error::{HttpServiceError, TimeoutError},
    http::{Request, RequestExt, Response},
    service::HttpService,
    util::timer::Timeout,
};

use super::{body::RequestBody, proto::Dispatcher};

pub type H2Service<St, S, A, const HEADER_LIMIT: usize, const READ_BUF_LIMIT: usize, const WRITE_BUF_LIMIT: usize> =
    HttpService<St, S, RequestBody, A, HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>;

impl<St, S, ResB, BE, A, TlsSt, const HEADER_LIMIT: usize, const READ_BUF_LIMIT: usize, const WRITE_BUF_LIMIT: usize>
    Service<(St, SocketAddr)> for H2Service<St, S, A, HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>
where
    S: Service<Request<RequestExt<RequestBody>>, Response = Response<ResB>> + 'static,
    S::Error: fmt::Debug,
    A: Service<St, Response = TlsSt>,
    St: AsyncIo,
    TlsSt: AsyncIo,
    HttpServiceError<S::Error, BE>: From<A::Error>,
    ResB: Stream<Item = Result<Bytes, BE>> + Send + 'static,
    BE: fmt::Debug + Send + 'static,
{
    type Response = ();
    type Error = HttpServiceError<S::Error, BE>;

    async fn call(&self, (io, addr): (St, SocketAddr)) -> Result<Self::Response, Self::Error> {
        // tls accept timer.
        let timer = self.keep_alive();
        let mut timer = pin!(timer);

        let tls_stream = self
            .tls_acceptor
            .call(io)
            .timeout(timer.as_mut())
            .await
            .map_err(|_| HttpServiceError::Timeout(TimeoutError::TlsAccept))??;

        // update timer to first request timeout.
        self.update_first_request_deadline(timer.as_mut());

        let mut conn = {
            let mut b = ::h2::server::Builder::new();
            b.enable_connect_protocol()
                // 连接级接收窗口：128MB
                // BDP = 带宽 × RTT，千兆网卡 RTT=20ms → BDP=2.5MB，128MB 足够跑满多路并发流
                .initial_connection_window_size(128 * 1024 * 1024)
                // 流级接收窗口：16MB，单个大文件流不因 WINDOW_UPDATE 频繁停顿
                .initial_window_size(16 * 1024 * 1024)
                // 最大并发流：1000
                .max_concurrent_streams(1000)
                // 最大帧：1MB（规范允许最大 16MB）
                // 大文件时 1MB chunk 只需 1 个帧，framer 调度开销降低 64 倍
                // 对标 Cloudflare/Caddy 的大帧策略
                .max_frame_size(1024 * 1024)
                // 最大头部列表：32KB
                .max_header_list_size(32768)
                // RST 洪水防护
                .max_concurrent_reset_streams(1000)
                // 发送缓冲：16MB，防止大文件发送时生产者频繁等待
                .max_send_buffer_size(16 * 1024 * 1024);
            b.handshake(PollIoAdapter(tls_stream))
        }
        .timeout(timer.as_mut())
        .await
        .map_err(|_| HttpServiceError::Timeout(TimeoutError::H2Handshake))??;

        let dispatcher = Dispatcher::new(
            &mut conn,
            addr,
            true, // H2 service 只在 TLS accept 后创建
            timer,
            self.config.keep_alive_timeout,
            std::sync::Arc::clone(&self.service),
            self.date.get_rc(),
        );

        dispatcher.run().await?;

        Ok(())
    }
}

#[cfg(feature = "io-uring")]
pub(crate) use io_uring::H2UringService;

#[cfg(feature = "io-uring")]
mod io_uring {
    use {
        sweety_io_compat::{
            io_uring::{AsyncBufRead, AsyncBufWrite},
            net::io_uring::TcpStream,
        },
        sweety_service::ready::ReadyService,
    };

    use crate::{
        config::HttpServiceConfig,
        date::{DateTime, DateTimeService},
        util::timer::KeepAlive,
    };

    use super::*;

    pub struct H2UringService<
        S,
        A,
        const HEADER_LIMIT: usize,
        const READ_BUF_LIMIT: usize,
        const WRITE_BUF_LIMIT: usize,
    > {
        pub(crate) config: HttpServiceConfig<HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>,
        pub(crate) date: DateTimeService,
        pub(crate) service: S,
        pub(crate) tls_acceptor: A,
    }

    impl<S, A, const HEADER_LIMIT: usize, const READ_BUF_LIMIT: usize, const WRITE_BUF_LIMIT: usize>
        H2UringService<S, A, HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>
    {
        pub(crate) fn new(
            config: HttpServiceConfig<HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>,
            service: S,
            tls_acceptor: A,
        ) -> Self {
            Self {
                config,
                date: DateTimeService::new(),
                service,
                tls_acceptor,
            }
        }
    }

    impl<S, B, BE, A, const HEADER_LIMIT: usize, const READ_BUF_LIMIT: usize, const WRITE_BUF_LIMIT: usize>
        Service<(TcpStream, SocketAddr)> for H2UringService<S, A, HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>
    where
        S: Service<Request<RequestExt<crate::h2::proto::RequestBody>>, Response = Response<B>>,
        A: Service<TcpStream>,
        A::Response: AsyncBufRead + AsyncBufWrite + 'static,
        B: Stream<Item = Result<Bytes, BE>>,
        HttpServiceError<S::Error, BE>: From<A::Error>,
        S::Error: fmt::Debug,
        BE: fmt::Debug,
    {
        type Response = ();
        type Error = HttpServiceError<S::Error, BE>;
        async fn call(&self, (io, _): (TcpStream, SocketAddr)) -> Result<Self::Response, Self::Error> {
            let accept_dur = self.config.tls_accept_timeout;
            let deadline = self.date.get().now() + accept_dur;
            let mut timer = pin!(KeepAlive::new(deadline));

            let io = self
                .tls_acceptor
                .call(io)
                .timeout(timer.as_mut())
                .await
                .map_err(|_| HttpServiceError::Timeout(TimeoutError::TlsAccept))??;

            crate::h2::proto::run(io, &self.service).await.unwrap();

            Ok(())
        }
    }

    impl<S, A, const HEADER_LIMIT: usize, const READ_BUF_LIMIT: usize, const WRITE_BUF_LIMIT: usize> ReadyService
        for H2UringService<S, A, HEADER_LIMIT, READ_BUF_LIMIT, WRITE_BUF_LIMIT>
    where
        S: ReadyService,
    {
        type Ready = S::Ready;

        #[inline]
        async fn ready(&self) -> Self::Ready {
            self.service.ready().await
        }
    }
}
