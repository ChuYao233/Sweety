//! error types.

use std::{
    convert::Infallible,
    error::Error,
    fmt::{self, Debug, Formatter},
};

use tracing::{debug, error};

use super::http::Version;

pub(crate) use super::tls::TlsError;

/// HttpService layer error.
pub enum HttpServiceError<S, B> {
    Ignored,
    Service(S),
    Body(B),
    Timeout(TimeoutError),
    UnSupportedVersion(Version),
    Tls(TlsError),
    #[cfg(feature = "http1")]
    H1(super::h1::Error<S, B>),
    // Http/2 error happen in HttpService handle.
    #[cfg(feature = "http2")]
    H2(super::h2::Error<S, B>),
    // Http/3 error happen in HttpService handle.
    #[cfg(feature = "http3")]
    H3(super::h3::Error<S, B>),
}

impl<S, B> Debug for HttpServiceError<S, B>
where
    S: Debug,
    B: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Ignored => write!(f, "Error detail is ignored."),
            Self::Service(ref e) => Debug::fmt(e, f),
            Self::Timeout(ref timeout) => write!(f, "{timeout:?} is timed out"),
            Self::UnSupportedVersion(ref protocol) => write!(f, "Protocol: {protocol:?} is not supported"),
            Self::Body(ref e) => Debug::fmt(e, f),
            Self::Tls(ref e) => Debug::fmt(e, f),
            #[cfg(feature = "http1")]
            Self::H1(ref e) => Debug::fmt(e, f),
            #[cfg(feature = "http2")]
            Self::H2(ref e) => Debug::fmt(e, f),
            #[cfg(feature = "http3")]
            Self::H3(ref e) => Debug::fmt(e, f),
        }
    }
}

impl<S, B> HttpServiceError<S, B>
where
    S: Debug,
    B: Debug,
{
    pub fn log(self, target: &str) {
        match &self {
            // 客户端主动断开、RST、CANCEL 属于正常行为，只记 debug
            Self::Ignored => {}
            Self::Timeout(_) => debug!(target = target, ?self, "connection timed out"),
            #[cfg(feature = "http2")]
            Self::H2(super::h2::Error::H2(e)) if is_h2_client_reset(e) => {
                debug!(target = target, ?self, "h2 client reset or cancel")
            }
            // TLS 握手失败（扫描器/非 TLS 客户端连接）也只记 debug
            Self::Tls(_) => debug!(target = target, ?self, "tls handshake failed"),
            // 其他真正的服务错误记 error
            _ => error!(target = target, ?self),
        }
    }
}

/// 判断 h2::Error 是否属于客户端主动触发（CANCEL/REFUSED/GO_AWAY 等）
#[cfg(feature = "http2")]
#[inline]
fn is_h2_client_reset(e: &::h2::Error) -> bool {
    if e.is_remote() {
        return true;
    }
    if let Some(reason) = e.reason() {
        matches!(
            reason,
            ::h2::Reason::CANCEL
            | ::h2::Reason::REFUSED_STREAM
            | ::h2::Reason::NO_ERROR
            | ::h2::Reason::STREAM_CLOSED
        )
    } else {
        false
    }
}

/// time out error from async task that run for too long.
#[derive(Debug)]
pub enum TimeoutError {
    TlsAccept,
    #[cfg(feature = "http2")]
    H2Handshake,
}

impl<S, B> From<()> for HttpServiceError<S, B> {
    fn from(_: ()) -> Self {
        Self::Ignored
    }
}

impl<S, B> From<Infallible> for HttpServiceError<S, B> {
    fn from(e: Infallible) -> Self {
        match e {}
    }
}

/// Default Request/Response body error.
pub type BodyError = Box<dyn Error + Send + Sync>;
