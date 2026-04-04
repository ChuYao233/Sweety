//! 管理 API 模块（对标 Caddy Admin API 全部功能，并扩展更多端点）
//! 提供 HTTP RESTful 接口和 WebSocket 实时推送，用于运行时动态管理
//!
//! ## 模块结构
//! - `context` — AdminContext 共享上下文
//! - `server`  — TCP 监听、HTTP/1.1 解析、鉴权
//! - `router`  — 路由分发
//! - `handlers/` — 各端点实现（system / config / sites / upstreams / runtime）
//! - `doc`     — API 文档生成
//! - `util`    — 工具函数（JSON 响应、URL 解析等）

pub mod context;
pub mod server;
pub mod router;
pub mod handlers;
pub mod doc;
pub mod util;
pub mod websocket;

// re-export 供外部使用
pub use context::AdminContext;
pub use server::start;
pub use doc::build_api_doc;
