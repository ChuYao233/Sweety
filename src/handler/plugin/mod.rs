//! 插件系统
//!
//! # 接入方式（配置）
//! ```toml
//! [[site.location]]
//! path    = "/api/"
//! handler = "plugin:my_waf"   # 格式：plugin:<name>
//! ```
//!
//! # 实现插件
//! 1. 实现 `Plugin` trait
//! 2. 调用 `plugin_registry().register("my_waf", Arc::new(MyWaf))`
//!    （通常在 `main.rs` 启动阶段完成）
//!
//! # 设计原则
//! - 热路径零开销：未注册插件时 `lookup` 返回 None，dispatch 路径完全跳过
//! - 插件本身是 `Arc<dyn Plugin + Send + Sync>`，可跨 task 共享
//! - 插件可挂载在 request 阶段（修改/拒绝请求）或 response 阶段（修改响应）
//! - 支持短路：`PluginResult::Stop(resp)` 直接返回响应，不继续走后续 handler

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use sweety_web::http::{WebResponse, header::HeaderMap};

// ─────────────────────────────────────────────
// 插件 trait
// ─────────────────────────────────────────────

/// 插件请求上下文（传给插件的只读视图，零拷贝）
pub struct PluginRequest<'a> {
    /// HTTP 方法
    pub method:      &'a str,
    /// 请求路径（含 query）
    pub path:        &'a str,
    /// 请求头
    pub headers:     &'a HeaderMap,
    /// 客户端 IP
    pub client_ip:   &'a str,
    /// 请求体字节数（不含实际内容，避免大 body 拷贝）
    pub body_len:    usize,
}

/// 插件处理结果
pub enum PluginResult {
    /// 继续走后续 handler
    Continue,
    /// 短路：直接返回此响应
    Stop(WebResponse),
}

/// 插件 trait
///
/// 所有方法都有默认实现（返回 Continue），插件只需覆盖关心的阶段。
pub trait Plugin: Send + Sync {
    /// 请求阶段钩子：在路由匹配后、handler 执行前调用
    ///
    /// 可用于：WAF 过滤、自定义认证、限流、日志等
    fn on_request(&self, req: &PluginRequest<'_>) -> PluginResult {
        let _ = req;
        PluginResult::Continue
    }

    /// 响应阶段钩子：在 handler 返回响应后调用
    ///
    /// 可用于：添加/修改响应头、响应体改写、监控打点等
    fn on_response(&self, resp: WebResponse) -> WebResponse {
        resp
    }

    /// 插件名称（用于日志/调试）
    fn name(&self) -> &'static str;
}

// ─────────────────────────────────────────────
// 全局注册表
// ─────────────────────────────────────────────

/// 插件注册表（全局单例）
#[derive(Default)]
pub struct PluginRegistry {
    plugins: RwLock<HashMap<String, Arc<dyn Plugin>>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册插件（通常在 main.rs 启动阶段调用）
    pub fn register(&self, name: impl Into<String>, plugin: Arc<dyn Plugin>) {
        self.plugins.write().unwrap_or_else(|e| e.into_inner()).insert(name.into(), plugin);
    }

    /// 按名称查找插件（热路径：读锁，O(1) HashMap 查找）
    #[inline]
    pub fn lookup(&self, name: &str) -> Option<Arc<dyn Plugin>> {
        self.plugins.read().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    /// 返回所有已注册插件名（用于 /api/v1/plugins 和 --api-doc）
    pub fn plugin_names(&self) -> Vec<String> {
        self.plugins.read().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect()
    }
}

/// 全局插件注册表单例
pub static PLUGIN_REGISTRY: std::sync::LazyLock<PluginRegistry> =
    std::sync::LazyLock::new(PluginRegistry::new);

/// 获取全局插件注册表
#[inline(always)]
pub fn plugin_registry() -> &'static PluginRegistry {
    &PLUGIN_REGISTRY
}

// ─────────────────────────────────────────────
// RequestHandler trait（完整 handler 注册）
// ─────────────────────────────────────────────

/// 完整请求处理上下文（传给自定义 handler）
pub struct HandlerContext<'a> {
    /// HTTP 方法
    pub method:    &'a str,
    /// 请求路径（含 query）
    pub path:      &'a str,
    /// 请求头
    pub headers:   &'a sweety_web::http::header::HeaderMap,
    /// 客户端 IP
    pub client_ip: &'a str,
    /// 站点名称
    pub site_name: &'a str,
    /// Location 路径匹配模式
    pub location_path: &'a str,
}

/// 自定义请求 Handler trait
///
/// 实现此 trait 并注册到 `handler_registry()` 后，
/// 在配置中用 `handler = "plugin:my_handler"` 调用。
///
/// # 示例
/// ```rust
/// use sweety_lib::handler::plugin::{RequestHandler, HandlerContext, handler_registry};
/// use sweety_web::http::{WebResponse, StatusCode};
/// use sweety_web::body::ResponseBody;
/// use std::sync::Arc;
///
/// struct MyHandler;
///
/// impl RequestHandler for MyHandler {
///     fn name(&self) -> &'static str { "my_handler" }
///
///     fn handle<'a>(&'a self, ctx: HandlerContext<'a>)
///         -> std::pin::Pin<Box<dyn std::future::Future<Output = WebResponse> + Send + 'a>>
///     {
///         Box::pin(async move {
///             let mut resp = WebResponse::new(ResponseBody::none());
///             *resp.status_mut() = StatusCode::OK;
///             resp
///         })
///     }
/// }
///
/// // main.rs 启动时注册
/// handler_registry().register("my_handler", Arc::new(MyHandler));
/// ```
pub trait RequestHandler: Send + Sync {
    /// handler 名称（唯一标识）
    fn name(&self) -> &'static str;

    /// 处理请求，返回响应
    ///
    /// 使用 `Box::pin(async move { ... })` 包裹 async 逻辑
    fn handle<'a>(
        &'a self,
        ctx: HandlerContext<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = WebResponse> + Send + 'a>>;
}

/// 自定义 handler 注册表（全局单例）
#[derive(Default)]
pub struct HandlerRegistry {
    handlers: RwLock<HashMap<String, Arc<dyn RequestHandler>>>,
}

impl HandlerRegistry {
    pub fn new() -> Self { Self::default() }

    /// 注册自定义 handler
    pub fn register(&self, name: impl Into<String>, handler: Arc<dyn RequestHandler>) {
        self.handlers.write().unwrap_or_else(|e| e.into_inner()).insert(name.into(), handler);
    }

    /// 查找已注册 handler
    #[inline]
    pub fn lookup(&self, name: &str) -> Option<Arc<dyn RequestHandler>> {
        self.handlers.read().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
    }

    /// 返回所有注册的 handler 名（Admin API 用）
    pub fn handler_names(&self) -> Vec<String> {
        self.handlers.read().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect()
    }
}

/// 全局 handler 注册表单例
pub static HANDLER_REGISTRY: std::sync::LazyLock<HandlerRegistry> =
    std::sync::LazyLock::new(HandlerRegistry::new);

/// 获取全局 handler 注册表
#[inline(always)]
pub fn handler_registry() -> &'static HandlerRegistry {
    &HANDLER_REGISTRY
}

/// 尝试用注册的自定义 handler 处理请求
///
/// 返回 `Some(resp)` 表示找到 handler 并处理了；`None` 表示未注册，继续走内置逻辑
#[inline]
pub async fn run_custom_handler(handler_name: &str, ctx: HandlerContext<'_>) -> Option<WebResponse> {
    let handler = handler_registry().lookup(handler_name)?;
    Some(handler.handle(ctx).await)
}

// ─────────────────────────────────────────────
// 配置解析辅助
// ─────────────────────────────────────────────

/// 从 handler 字符串解析插件名
///
/// `"plugin:my_waf"` → `Some("my_waf")`
/// `"reverse_proxy"` → `None`
#[inline]
pub fn parse_plugin_name(handler: &str) -> Option<&str> {
    handler.strip_prefix("plugin:")
}

// ─────────────────────────────────────────────
// dispatch 辅助：在 handler 分发前统一调用
// ─────────────────────────────────────────────

/// 执行插件 on_request 钩子
///
/// 返回 `Some(resp)` 表示插件短路，直接返回；`None` 表示继续。
///
/// 设计保证：
/// - 没有插件注册时此函数直接返回 None，热路径无任何锁争用
/// - 插件 Arc clone 只在命中时发生（最多一次 Arc::clone）
#[inline]
pub fn run_plugin_request(plugin_name: &str, req: &PluginRequest<'_>) -> Option<WebResponse> {
    let plugin = plugin_registry().lookup(plugin_name)?;
    match plugin.on_request(req) {
        PluginResult::Continue    => None,
        PluginResult::Stop(resp)  => Some(resp),
    }
}

/// 执行插件 on_response 钩子
#[inline]
pub fn run_plugin_response(plugin_name: &str, resp: WebResponse) -> WebResponse {
    match plugin_registry().lookup(plugin_name) {
        Some(p) => p.on_response(resp),
        None    => resp,
    }
}
