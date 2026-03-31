//! HttpEngine trait：HTTP 引擎抽象
//!
//! Sweety 核心逻辑只依赖此 trait，不依赖具体实现。
//! 当前唯一实现：XitcaEngine（底层为 xitca-web）。
//! 未来可替换为自研 IO 层而无需改动业务代码。

use std::future::Future;

use anyhow::Result;

/// HTTP 引擎抽象 trait
///
/// 实现此 trait 的类型负责：
/// - 绑定 TCP/TLS/QUIC 端口
/// - Accept 连接
/// - 分发到 handler
/// - 优雅关闭
pub trait HttpEngine: Send + Sync + 'static {
    /// 运行引擎直到收到停止信号
    fn run(self) -> impl Future<Output = Result<()>> + Send;
}
