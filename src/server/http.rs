//! HTTP 服务器核心模块
//! 负责：基于 xitca-web 构建多站点、多协议（HTTP/1.1 + HTTP/2 + HTTP/3）服务器
//! 支持：明文 HTTP、TLS（rustls）、ACME 自动证书、HTTP/3（QUIC）

use std::io;
use std::sync::Arc;

use tracing::info;
use xitca_web::{
    App,
    body::ResponseBody,
    handler::handler_service,
    http::{StatusCode, WebResponse, header::{CONTENT_TYPE, LOCATION, HeaderValue}},
    middleware::Logger,
    route::get,
    WebContext,
};

use crate::config::model::{AppConfig, HandlerType};
use crate::dispatcher::vhost::VHostRegistry;
use crate::middleware::metrics::GlobalMetrics;
use crate::server::tls::TlsManager;

/// Sweety 服务器入口结构体
pub struct SweetyServer {
    cfg: AppConfig,
}

impl SweetyServer {
    pub fn new(cfg: AppConfig) -> Self {
        Self { cfg }
    }

    /// 启动服务器（阻塞直到收到停止信号）
    pub fn run(self) -> io::Result<()> {
        let cfg = Arc::new(self.cfg);
        let metrics = Arc::new(GlobalMetrics::new());
        let registry = Arc::new(VHostRegistry::from_config(&cfg.sites));

        // 构建共享应用状态
        let state = AppState {
            registry: registry.clone(),
            metrics: metrics.clone(),
            cfg: cfg.clone(),
        };

        // 构建 xitca-web App，使用多站点 dispatcher 作为根处理器
        // handler_service 的函数参数必须是 &WebContext 引用
        let app = App::new()
            .with_state(state.clone())
            .at("/*path", get(handler_service(multi_site_handler)).post(handler_service(multi_site_handler)))
            .at("/", get(handler_service(multi_site_handler)).post(handler_service(multi_site_handler)))
            .enclosed(Logger::new());

        let mut server = xitca_web::HttpServer::serve(app.finish());

        // 根据 worker 线程数配置并发
        let workers = if cfg.global.worker_threads == 0 {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
        } else {
            cfg.global.worker_threads
        };
        server = server.worker_threads(workers);

        // 绑定各站点监听端口（HTTP 明文）
        let http_ports = collect_http_ports(&cfg);
        for port in &http_ports {
            let addr = format!("0.0.0.0:{}", port);
            server = server.bind(&addr)?;
            info!("HTTP 监听: {}", addr);
        }

        // 绑定 TLS 端口（HTTPS + HTTP/2）
        let tls_ports = collect_tls_ports(&cfg);
        for (port, tls_cfg) in tls_ports {
            let addr = format!("0.0.0.0:{}", port);
            match TlsManager::build_server_config(tls_cfg) {
                Ok(rustls_cfg) => {
                    server = server.bind_rustls(&addr, rustls_cfg)?;
                    info!("HTTPS/HTTP2 监听: {}", addr);
                }
                Err(e) => {
                    tracing::warn!("TLS 配置加载失败（端口 {}）: {}，跳过该端口", port, e);
                }
            }
        }

        // 绑定 HTTP/3 QUIC 端口
        // 先构建好所有 H3 配置，再统一通过链式 bind_h3 绑定（避免所有权问题）
        let h3_bindings: Vec<(String, xitca_io::net::QuicConfig)> = collect_h3_ports(&cfg)
            .into_iter()
            .filter_map(|(port, tls_cfg)| {
                match TlsManager::build_quic_config(tls_cfg) {
                    Ok(cfg) => Some((format!("0.0.0.0:{}", port), cfg)),
                    Err(e) => {
                        tracing::warn!("HTTP/3 配置失败（端口 {}）: {}，跳过", port, e);
                        None
                    }
                }
            })
            .collect();

        for (addr, quic_cfg) in h3_bindings {
            server = server.bind_h3(&addr, quic_cfg)?;
            info!("HTTP/3 QUIC 监听: {}", addr);
        }

        // 启动 ACME 自动续期后台任务
        if cfg.sites.iter().any(|s| s.tls.as_ref().map(|t| t.acme).unwrap_or(false)) {
            let cfg_clone = cfg.clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(crate::server::tls::TlsManager::acme_renewal_loop(cfg_clone));
            });
        }

        // 运行服务器（阻塞）
        server.run().wait()
    }
}

/// 所有请求共享状态
#[derive(Clone)]
pub struct AppState {
    pub registry: Arc<VHostRegistry>,
    pub metrics: Arc<GlobalMetrics>,
    pub cfg: Arc<AppConfig>,
}

/// 多站点请求分发处理器
///
/// 参数必须是 `&WebContext` 引用（xitca-web handler_service FromRequest 约束）
async fn multi_site_handler(ctx: &WebContext<'_, AppState>) -> WebResponse {
    let state = ctx.state();
    state.metrics.inc_requests();

    // 提取 Host（去掉端口）
    let host = ctx
        .req()
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("")
        .to_string();

    let path = ctx.req().uri().path().to_string();

    // 虚拟主机查找
    let site = match state.registry.lookup(&host) {
        Some(s) => s.clone(),
        None => {
            state.metrics.record_status(404);
            return make_error_resp(StatusCode::NOT_FOUND);
        }
    };

    // 安全检查：拦截敏感路径
    if crate::middleware::security::is_sensitive_path(&path) {
        state.metrics.record_status(403);
        return make_error_resp(StatusCode::FORBIDDEN);
    }

    // Location 匹配 + Rewrite
    let rewritten = crate::dispatcher::rewrite::apply_rewrites(&site.rewrites, &path);

    // 处理重定向（Rewrite 引擎返回 REDIRECT:301:/new 格式）
    if let Some(ref rp) = rewritten {
        if let Some(rest) = rp.strip_prefix("REDIRECT:301:") {
            return make_redirect_resp(rest, StatusCode::MOVED_PERMANENTLY);
        }
        if let Some(rest) = rp.strip_prefix("REDIRECT:302:") {
            return make_redirect_resp(rest, StatusCode::FOUND);
        }
    }

    let effective_path = rewritten.as_deref().unwrap_or(&path);

    let location = match crate::dispatcher::location::match_location(&site.locations, effective_path) {
        Some(loc) => loc.clone(),
        None => {
            state.metrics.record_status(404);
            return make_error_resp(StatusCode::NOT_FOUND);
        }
    };

    // 直接返回状态码（健康检查）
    if let Some(code) = location.return_code {
        let status = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
        state.metrics.record_status(code);
        let mut resp = WebResponse::new(ResponseBody::empty());
        *resp.status_mut() = status;
        return resp;
    }

    // 根据 handler 类型分发
    let resp = match location.handler {
        HandlerType::Static => {
            crate::handler::static_file::handle_xitca(ctx, &site, &location).await
        }
        HandlerType::Fastcgi => {
            crate::handler::fastcgi::handle_xitca(ctx, &site, &location).await
        }
        HandlerType::Websocket => {
            crate::handler::websocket::handle_xitca(ctx, &location).await
        }
        HandlerType::ReverseProxy => {
            crate::handler::reverse_proxy::handle_xitca(ctx, &site, &location).await
        }
    };

    state.metrics.record_status(resp.status().as_u16());
    resp
}

/// 构造 HTML 错误响应（不依赖 ctx）
fn make_error_resp(status: StatusCode) -> WebResponse {
    let body = crate::handler::error_page::build_default_html(status.as_u16());
    let mut resp = WebResponse::new(ResponseBody::from(body));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp
}

/// 构造重定向响应
fn make_redirect_resp(location: &str, status: StatusCode) -> WebResponse {
    let mut resp = WebResponse::new(ResponseBody::empty());
    *resp.status_mut() = status;
    if let Ok(v) = HeaderValue::try_from(location) {
        resp.headers_mut().insert(LOCATION, v);
    }
    resp
}

/// 收集所有站点的 HTTP 端口（去重）
fn collect_http_ports(cfg: &AppConfig) -> Vec<u16> {
    let mut ports = std::collections::HashSet::new();
    for site in &cfg.sites {
        for &p in &site.listen {
            ports.insert(p);
        }
    }
    if ports.is_empty() {
        ports.insert(80);
    }
    let mut v: Vec<u16> = ports.into_iter().collect();
    v.sort_unstable();
    v
}

/// 收集所有站点的 TLS 端口及对应 TLS 配置
fn collect_tls_ports(cfg: &AppConfig) -> Vec<(u16, &crate::config::model::TlsConfig)> {
    let mut result = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for site in &cfg.sites {
        if let Some(tls) = &site.tls {
            for &p in &site.listen_tls {
                if seen.insert(p) {
                    result.push((p, tls));
                }
            }
        }
    }
    result
}

/// 收集 HTTP/3 QUIC 端口及对应 TLS 配置
fn collect_h3_ports(cfg: &AppConfig) -> Vec<(u16, &crate::config::model::TlsConfig)> {
    collect_tls_ports(cfg)
}
