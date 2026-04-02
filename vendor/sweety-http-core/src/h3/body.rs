use core::{
    mem::ManuallyDrop,
    pin::Pin,
    task::{Context, Poll},
};

use ::h3::server::RequestStream;
use futures_core::stream::Stream;
use h3_quinn::RecvStream;

use crate::{
    bytes::{Buf, Bytes},
    error::BodyError,
};

/// Request body type for Http/3 specifically.
///
/// 用 ManuallyDrop 包装 RequestStream，drop 时什么都不做——
/// RecvStream::drop 会发 STOP_SENDING，导致客户端回 RESET_STREAM，
/// h2load 标记 errored（即使响应已完整收到）。
/// quinn 连接关闭时统一回收所有流资源，每个流泄漏量极小（几十字节）。
pub struct RequestBody(ManuallyDrop<RequestStream<RecvStream, Bytes>>);

impl RequestBody {
    pub(in crate::h3) fn new(rx: RequestStream<RecvStream, Bytes>) -> Self {
        Self(ManuallyDrop::new(rx))
    }
}

impl Drop for RequestBody {
    fn drop(&mut self) {
        // 故意不 drop 内部 RequestStream，防止 RecvStream::drop 发 STOP_SENDING
    }
}

impl Stream for RequestBody {
    type Item = Result<Bytes, BodyError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let rx = &mut self.get_mut().0;
        rx.poll_recv_data(cx)?
            .map(|res| res.map(|buf| Ok(Bytes::copy_from_slice(buf.chunk()))))
    }
}

impl From<RequestBody> for crate::body::RequestBody {
    fn from(body: RequestBody) -> Self {
        Self::H3(body)
    }
}
