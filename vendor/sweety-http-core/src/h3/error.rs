use h3_quinn::quinn::ConnectionError;

use crate::error::HttpServiceError;

#[derive(Debug)]
pub enum Error<S, B> {
    Service(S),
    Body(B),
    Connection(ConnectionError),
    // h3 连接级错误（accept 返回）
    H3(::h3::error::ConnectionError),
    // h3 流级错误（send_response/send_data/resolve_request 返回）
    Stream(::h3::error::StreamError),
}

impl<S, B> From<::h3::error::ConnectionError> for Error<S, B> {
    fn from(e: ::h3::error::ConnectionError) -> Self {
        Self::H3(e)
    }
}

impl<S, B> From<::h3::error::StreamError> for Error<S, B> {
    fn from(e: ::h3::error::StreamError) -> Self {
        Self::Stream(e)
    }
}

impl<S, B> From<ConnectionError> for Error<S, B> {
    fn from(e: ConnectionError) -> Self {
        Self::Connection(e)
    }
}

impl<S, B> From<Error<S, B>> for HttpServiceError<S, B> {
    fn from(e: Error<S, B>) -> Self {
        match e {
            Error::Service(e) => Self::Service(e),
            e => Self::H3(e),
        }
    }
}
