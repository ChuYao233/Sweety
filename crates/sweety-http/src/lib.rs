//! sweety-http：HTTP 引擎抽象层
//!
//! 定义 [`HttpEngine`] trait，Sweety 核心逻辑通过此 trait 驱动 HTTP 服务。
//! 底层实现（当前为 xitca-web）完全隔离在本 crate 内部。

pub mod engine;
pub mod builder;

pub use builder::ServerBuilder;
pub use engine::HttpEngine;
