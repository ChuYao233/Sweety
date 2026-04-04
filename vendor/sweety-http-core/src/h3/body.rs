use core::{
    pin::Pin,
    task::{Context, Poll},
};
use std::sync::Arc;
use std::task::Wake;

use ::h3::server::RequestStream;
use futures_core::stream::Stream;
use h3_quinn::RecvStream;

use crate::{
    bytes::{Buf, Bytes},
    error::BodyError,
};

/// Request body type for Http/3 specifically.
///
/// RequestBody 在 Drop 时会尽力把可立即读取的数据 drain 到 EOF。
/// - 若能同步到 EOF：正常释放流（回收 stream 并发额度）
/// - 若仍 Pending：保持旧行为，不主动 STOP_SENDING（避免客户端误判 errored）
pub struct RequestBody(Option<RequestStream<RecvStream, Bytes>>);

impl RequestBody {
    pub(in crate::h3) fn new(rx: RequestStream<RecvStream, Bytes>) -> Self {
        Self(Some(rx))
    }
}

impl Drop for RequestBody {
    fn drop(&mut self) {
        struct NoopWake;
        impl Wake for NoopWake {
            fn wake(self: Arc<Self>) {}
        }

        let Some(mut rx) = self.0.take() else { return; };
        let waker: std::task::Waker = Arc::new(NoopWake).into();
        let mut cx = Context::from_waker(&waker);

        // 尽可能把已就绪数据 drain 掉：GET/HEAD 空 body 场景通常可立即到 EOF
        loop {
            match Pin::new(&mut rx).poll_recv_data(&mut cx) {
                Poll::Ready(Ok(Some(_))) => continue,
                Poll::Ready(Ok(None)) => {
                    // 已到 EOF，正常 drop 回收 stream 额度
                    return;
                }
                Poll::Ready(Err(_)) | Poll::Pending => {
                    // 正常 drop：对 GET（无 body）客户端已 FIN，STOP_SENDING 是 no-op；
                    // 对 POST（有未读 body），STOP_SENDING 是正确的协议行为。
                    // 注意：不能用 mem::forget，否则 quinn connection Arc 引用永不归零，导致 OOM。
                    drop(rx);
                    return;
                }
            }
        }
    }
}

impl Stream for RequestBody {
    type Item = Result<Bytes, BodyError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Some(rx) = self.get_mut().0.as_mut() else {
            return Poll::Ready(None);
        };
        rx.poll_recv_data(cx)?
            .map(|res| res.map(|buf| Ok(Bytes::copy_from_slice(buf.chunk()))))
    }
}

impl From<RequestBody> for crate::body::RequestBody {
    fn from(body: RequestBody) -> Self {
        Self::H3(body)
    }
}
