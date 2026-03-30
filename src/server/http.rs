//! HTTP 服务器核心模块
//! 负责：基于 xitca-web 构建多站点、多协议（HTTP/1.1 + HTTP/2 + HTTP/3）服务器
//! 支持：明文 HTTP、TLS（rustls）、ACME 自动证书、HTTP/3（QUIC）

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use tracing::info;
use xitca_web::{
    App,
    body::ResponseBody,
    handler::handler_service,
    http::{StatusCode, WebResponse, header::{CONTENT_TYPE, LOCATION, HeaderValue}},
    route::get,
    WebContext,
};


use crate::config::model::{AppConfig, HandlerType};
use crate::config::hot_reload::{HotReloadContext, start_hot_reload};
use crate::dispatcher::vhost::VHostRegistry;
use crate::handler::reverse_proxy::pool::ConnPool;
use crate::middleware::access_log::{AccessLogEntry, AccessLogger, LogFormat};
use crate::middleware::metrics::GlobalMetrics;
use crate::middleware::proxy_cache::ProxyCache;
use crate::server::tls::{SniResolver, TlsManager};

/// Sweety 服务器入口结构体
pub struct SweetyServer {
    cfg: AppConfig,
    /// 配置文件路径（热重载监听用）
    config_path: Option<PathBuf>,
}

impl SweetyServer {
    pub fn new(cfg: AppConfig) -> Self {
        Self { cfg, config_path: None }
    }

    /// 指定配置文件路径，用于启动热重载监听
    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = Some(path);
        self
    }

    /// 启动服务器（阻塞直到收到停止信号）
    pub fn run(self) -> io::Result<()> {
        let cfg = Arc::new(self.cfg);
        let metrics = Arc::new(GlobalMetrics::new());
        let registry = Arc::new(VHostRegistry::from_config(&cfg.sites));

        // 收集所有 TLS 端口（用于 HSTS 判断）
        let tls_port_set: HashSet<u16> = cfg.sites.iter()
            .flat_map(|s| s.listen_tls.iter().copied())
            .collect();

        // 上游连接池：idle 连接数 = worker_connections / 128（兼顾并发与内存），keepalive_timeout 秒超时
        let pool_idle = (cfg.global.worker_connections / 128).max(8).min(256);
        let conn_pool = ConnPool::new(pool_idle, cfg.global.keepalive_timeout);

        // 第一步：收集所有端口的 TLS 配置和 SniResolver（key=端口号，热重载按端口精确更新）
        let mut port_resolvers: HashMap<u16, Arc<SniResolver>> = HashMap::new();
        let mut tls_bindings: Vec<(String, rustls::ServerConfig)> = Vec::new();
        for (port, sites_for_port) in collect_tls_ports_grouped(&cfg) {
            let addr = format!("0.0.0.0:{}", port);
            let site_refs: Vec<&crate::config::model::SiteConfig> =
                sites_for_port.iter().map(|s| s.as_ref()).collect();
            match TlsManager::build_sni_server_config(&site_refs) {
                Ok((rustls_cfg, resolver)) => {
                    port_resolvers.insert(port, resolver);
                    tls_bindings.push((addr, rustls_cfg));
                    info!("HTTPS/HTTP2 监听: 0.0.0.0:{} ({} 个站点/证书)", port, site_refs.len());
                }
                Err(e) => {
                    tracing::warn!("TLS 配置加载失败（端口 {}）: {}，跳过该端口", port, e);
                }
            }
        }

        // HTTP 端口集合（用于判断请求是否是 HTTP）
        let http_port_set: HashSet<u16> = collect_http_ports(&cfg).into_iter().collect();

        // 为各站点创建访问日志写入器（同步打开文件，避免在运行时内 block_on）
        let access_loggers = {
            let mut map: HashMap<String, Arc<AccessLogger>> = HashMap::new();
            for site in &cfg.sites {
                if let Some(log_path) = &site.access_log {
                    match AccessLogger::file_sync(log_path, LogFormat::Combined) {
                        Ok(l) => {
                            info!("站点 '{}' 访问日志: {}", site.name, log_path.display());
                            map.insert(site.name.clone(), Arc::new(l));
                        }
                        Err(e) => tracing::warn!("站点 '{}' 访问日志初始化失败: {}", site.name, e),
                    }
                }
            }
            Arc::new(map)
        };

        // 按站点配置创建反代缓存实例
        let proxy_caches = {
            let mut map: HashMap<String, Arc<ProxyCache>> = HashMap::new();
            for site in &cfg.sites {
                if let Some(cache_cfg) = &site.proxy_cache {
                    let cache = ProxyCache::from_config(cache_cfg);
                    map.insert(site.name.clone(), cache);
                    info!("站点 '{}' 反代缓存已开启（max_entries={}, ttl={}s）",
                        site.name, cache_cfg.max_entries, cache_cfg.ttl);
                }
            }
            Arc::new(map)
        };

        // 第二步：构建 state
        let state = AppState {
            registry: registry.clone(),
            metrics: metrics.clone(),
            cfg: cfg.clone(),
            tls_ports: Arc::new(tls_port_set),
            http_ports: Arc::new(http_port_set),
            conn_pool,
            sni_resolvers: Arc::new(port_resolvers),
            access_loggers,
            proxy_caches,
        };

        // 第三步：构建 xitca-web App
        let app = App::new()
            .with_state(state.clone())
            .at("/*path", get(handler_service(multi_site_handler)).post(handler_service(multi_site_handler)))
            .at("/", get(handler_service(multi_site_handler)).post(handler_service(multi_site_handler)));

        let mut server = xitca_web::HttpServer::serve(app.finish());

        // 根据 worker 线程数配置并发
        let workers = if cfg.global.worker_threads == 0 {
            std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4)
        } else {
            cfg.global.worker_threads
        };
        server = server.worker_threads(workers);

        // Keep-Alive 超时：连接空闲超时后强制关闭，释放 File 句柄和读缓冲
        // 等价于 Nginx keepalive_timeout，防止下载器断开后资源残留
        let ka_timeout = std::time::Duration::from_secs(
            if cfg.global.keepalive_timeout == 0 { 60 } else { cfg.global.keepalive_timeout }
        );
        server = server.keep_alive_timeout(ka_timeout);

        // 绑定各站点监听端口（HTTP 明文）
        let http_ports = collect_http_ports(&cfg);
        for port in &http_ports {
            let addr = format!("0.0.0.0:{}", port);
            server = server.bind(&addr)?;
            info!("HTTP 监听: {}", addr);
        }

        // 绑定 TLS 端口
        for (addr, rustls_cfg) in tls_bindings {
            server = server.bind_rustls(&addr, rustls_cfg)?;
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
            // 克隆 sni_resolvers 供 ACME 续期后热重载证书
            let resolvers_for_acme: HashMap<u16, Arc<SniResolver>> = (*state.sni_resolvers).clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(crate::server::tls::TlsManager::acme_renewal_loop(
                    cfg_clone,
                    resolvers_for_acme,
                ));
            });
        }

        // 启动配置热重载后台线程（监听配置文件及证书目录变更，只更新变化站点，不断连）
        if let Some(config_path) = self.config_path {
            // 从 state.sni_resolvers 克隆一份 HashMap（直接解包 Arc）
            let resolvers_map: HashMap<u16, Arc<SniResolver>> = (*state.sni_resolvers).clone();
            let hot_ctx = HotReloadContext {
                registry: registry.clone(),
                sni_resolvers: resolvers_map,
                port_sites: collect_port_sites(&cfg),
            };
            start_hot_reload(config_path, cfg.clone(), hot_ctx);
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
    /// HTTPS/TLS 端口集合
    pub tls_ports: Arc<HashSet<u16>>,
    /// HTTP 明文端口集合（O(1) 查找判断 HTTP/HTTPS，不依赖 scheme）
    pub http_ports: Arc<HashSet<u16>>,
    /// 上游 TCP/TLS 连接池（跨请求复用 idle 连接）
    pub conn_pool: ConnPool,
    /// SNI 证书 Resolver 按端口索引（热重载时原地更新证书，不断连）
    pub sni_resolvers: Arc<HashMap<u16, Arc<SniResolver>>>,
    /// 访问日志写入器（按站点名索引）
    pub access_loggers: Arc<HashMap<String, Arc<AccessLogger>>>,
    /// 反代响应缓存（按站点名索引）
    pub proxy_caches: Arc<HashMap<String, Arc<ProxyCache>>>,
}

/// 多站点请求分发处理器
///
/// 参数必须是 `&WebContext` 引用（xitca-web handler_service FromRequest 约束）
async fn multi_site_handler(ctx: &WebContext<'_, AppState>) -> WebResponse {
    let state = ctx.state();
    let req_start = std::time::Instant::now();
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
    // $request_uri 包含路径和查询字符串（等价 Nginx $request_uri）
    let request_uri = ctx.req().uri().path_and_query()
        .map(|pq| pq.as_str().to_string())
        .unwrap_or_else(|| path.clone());

    // 判断是否 HTTPS 请求
    // 策略：解析 Host 头中的端口号，查 http_ports 和 tls_ports。
    // Host 带端口：在 http_ports 里 = HTTP，在 tls_ports 里 = HTTPS。
    // Host 无端口：默认 80 属于 HTTP，443 属于 HTTPS；
    //   若两者都没配置，默认 HTTPS（居多数不带端口访问的即 443 HTTPS）。
    // O(1) HashSet 查找，高并发无开销。
    let host_port: Option<u16> = ctx.req().headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.split(':').nth(1))
        .and_then(|p| p.parse().ok());
    // URI scheme（H2/H3 有效，H1 通常为 None）
    let uri_scheme = ctx.req().uri().scheme_str();
    let is_https = match uri_scheme {
        Some("https") => true,
        Some("http")  => false,
        _ => match host_port {
            // Host 带端口：在 http_ports 里 = HTTP，否则 = HTTPS
            Some(p) => !state.http_ports.contains(&p),
            // Host 无端口 + H1：无法可靠判断，默认视为 HTTPS
            // （tls_ports 有端口说明站点有 TLS，无端口访问通常是通过浏览器直接输入 https://）
            None => !state.http_ports.is_empty() && state.tls_ports.contains(&443),
        },
    };

    // HTTPS 请求严格匹配（防跨站）：无精确/通配符匹配时返回 421 Misdirected Request
    // HTTP 请求允许 fallback 到默认站点（与 Nginx 行为一致）
    let site = if is_https {
        match state.registry.lookup_strict(&host) {
            Some(s) => s,
            None => {
                state.metrics.record_status(421);
                return make_error_resp(StatusCode::MISDIRECTED_REQUEST);
            }
        }
    } else {
        match state.registry.lookup(&host) {
            Some(s) => s,
            None => {
                state.metrics.record_status(404);
                return make_error_resp(StatusCode::NOT_FOUND);
            }
        }
    };

    // force_https：与 Nginx 行为完全一致
    // Nginx 的 return 301 https://... 写在 HTTP server block 里，HTTPS 连接根本不会进入那个 block。
    // Sweety 等效实现：只对 Host 头明确带了 HTTP 端口（如 :80）的请求跳转，
    // 无端口的请求不跳转（无法可靠判断协议，避免死循环）。
    // 带端口访问（http://ip:80 → Host: ip:80）完全可靠。
    // 无端口直接访问（http://ip → Host: ip，H1）无法区分，不处理，由用户在 DNS/CDN 层做强制 HTTPS。
    if site.force_https {
        let should_redirect = match uri_scheme {
            Some("http") => true,   // H2/H3 明确 scheme=http
            Some("https") => false, // H2/H3 明确 scheme=https
            _ => {
                // H1 无 scheme：只有 Host 头明确带了 http_port 才跳转
                match host_port {
                    Some(p) => state.http_ports.contains(&p),
                    None => false,  // 无端口：不跳转，无法可靠判断
                }
            }
        };
        if should_redirect {
            let tls_port = if site.listen_tls.contains(&443) { 443 }
                           else { site.listen_tls.first().copied().unwrap_or(443) };
            let host_for_redirect = if tls_port == 443 { host.clone() }
                                    else { format!("{}:{}", host, tls_port) };
            let redirect_url = format!("https://{}{}",
                host_for_redirect,
                ctx.req().uri().path_and_query().map(|pq| pq.as_str()).unwrap_or("/"));
            state.metrics.record_status(301);
            return make_redirect_resp(&redirect_url, StatusCode::MOVED_PERMANENTLY);
        }
    }

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

    // 请求体大小限制（Content-Length 超过 client_max_body_size 时拒绝）
    let max_body_bytes = (state.cfg.global.client_max_body_size as u64) * 1024 * 1024;
    if max_body_bytes > 0 {
        if let Some(content_length) = ctx.req().headers()
            .get(xitca_web::http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
        {
            if content_length > max_body_bytes {
                state.metrics.record_status(413);
                return make_error_resp(StatusCode::PAYLOAD_TOO_LARGE);
            }
        }
    }

    // return_url：带 URL 的 return 指令（等价 Nginx return 301 https://...）
    // 格式: "301 https://..." 或 "302 https://..." 或直接 URL（默认 301）
    if let Some(ref ret) = location.return_url {
        let (code, url) = parse_return_directive(ret, &request_uri);
        state.metrics.record_status(code);
        return make_redirect_resp(&url, StatusCode::from_u16(code).unwrap_or(StatusCode::MOVED_PERMANENTLY));
    }

    // return_code：直接返回状态码（健康检查）
    if let Some(code) = location.return_code {
        let status = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
        state.metrics.record_status(code);
        let mut resp = WebResponse::new(ResponseBody::empty());
        *resp.status_mut() = status;
        return resp;
    }

    // 根据 handler 类型分发
    let mut resp = match location.handler {
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

    // error_page：自定义错误页（等价 Nginx error_page 404 /404.html）
    // 使用 tokio::fs::read 异步读取，不阻塞 worker thread
    let status_u16 = resp.status().as_u16();
    if (400..600).contains(&status_u16) {
        if let Some(ep_path) = site.error_pages.get(&status_u16) {
            if let Some(root) = location.root.as_ref().or(site.root.as_ref()) {
                let ep_file = root.join(ep_path.trim_start_matches('/'));
                if let Ok(content) = tokio::fs::read(&ep_file).await {
                    let mut ep_resp = WebResponse::new(ResponseBody::from(content));
                    *ep_resp.status_mut() = resp.status();
                    let ext = ep_file.extension().and_then(|e| e.to_str()).unwrap_or("html");
                    let mime = crate::middleware::cache::mime_type_for(ext);
                    if let Ok(v) = HeaderValue::from_str(mime) {
                        ep_resp.headers_mut().insert(CONTENT_TYPE, v);
                    }
                    return ep_resp;
                }
            }
        }
    }

    // 注入 HSTS 响应头（仅当 HTTPS 且站点配置了 hsts 时）
    // 兴趣：is_https 为 false 时也检查 Host 端口，兼容部分反代场景 scheme 未设置的情况
    let inject_hsts = is_https || {
        let host_val = ctx.req().headers()
            .get("host").and_then(|v| v.to_str().ok()).unwrap_or("");
        if let Some(p) = host_val.split(':').nth(1) {
            p.parse::<u16>().map(|p| state.tls_ports.contains(&p)).unwrap_or(false)
        } else { false }
    };
    if let Some(hsts) = &site.hsts {
        if inject_hsts && hsts.max_age > 0 {
            if let Ok(v) = HeaderValue::try_from(build_hsts_value(hsts)) {
                resp.headers_mut().insert(
                    xitca_web::http::header::HeaderName::from_static("strict-transport-security"),
                    v,
                );
            }
        }
    }

    // 访问日志：异步写文件（spawn 避免阻塞响应返回）
    if let Some(logger) = state.access_loggers.get(&site.name) {
        let logger = logger.clone();
        let client_ip = ctx.req().body().socket_addr().ip().to_string();
        let method = ctx.req().method().as_str().to_string();
        let uri = request_uri.clone();
        let http_version = format!("{:?}", ctx.req().version());
        let status = resp.status().as_u16();
        let bytes_sent = resp.headers()
            .get(xitca_web::http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0u64);
        let referer = ctx.req().headers()
            .get(xitca_web::http::header::REFERER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-")
            .to_string();
        let user_agent = ctx.req().headers()
            .get(xitca_web::http::header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-")
            .to_string();
        let site_name = site.name.clone();
        tokio::spawn(async move {
            logger.write(&AccessLogEntry {
                client_ip, method, uri, http_version,
                status, bytes_sent, referer, user_agent,
                duration_ms: req_start.elapsed().as_millis() as u64,
                site: site_name,
            }).await;
        });
    }

    resp
}

/// 构造 Strict-Transport-Security 头值
fn build_hsts_value(hsts: &crate::config::model::HstsConfig) -> String {
    let mut val = format!("max-age={}", hsts.max_age);
    if hsts.include_sub_domains {
        val.push_str("; includeSubDomains");
    }
    if hsts.preload {
        val.push_str("; preload");
    }
    val
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

/// 解析 return 指令："301 https://..." 或 "https://..."（默认 301）
/// 支持 $request_uri 变量替换
fn parse_return_directive(ret: &str, request_uri: &str) -> (u16, String) {
    let ret = ret.trim();
    // 尝试解析 "<code> <url>" 格式
    if let Some(space) = ret.find(' ') {
        let code_str = &ret[..space];
        let url = ret[space + 1..].trim()
            .replace("$request_uri", request_uri)
            .replace("$uri", request_uri.split('?').next().unwrap_or(request_uri));
        if let Ok(code) = code_str.parse::<u16>() {
            return (code, url);
        }
    }
    // 纯 URL，默认 301
    let url = ret
        .replace("$request_uri", request_uri)
        .replace("$uri", request_uri.split('?').next().unwrap_or(request_uri));
    (301, url)
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

/// 按 TLS 端口分组站点（同端口多站点 → SNI 多证书）
///
/// 返回：port → 该端口上所有有 TLS 配置的站点（按配置顺序）
fn collect_tls_ports_grouped(
    cfg: &AppConfig,
) -> Vec<(u16, Vec<std::sync::Arc<crate::config::model::SiteConfig>>)> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<u16, Vec<std::sync::Arc<crate::config::model::SiteConfig>>> =
        BTreeMap::new();
    for site in &cfg.sites {
        if site.tls.is_none() { continue; }
        let site_arc = std::sync::Arc::new(site.clone());
        for &p in &site.listen_tls {
            map.entry(p).or_default().push(site_arc.clone());
        }
    }
    map.into_iter().collect()
}

/// 收集 HTTP/3 QUIC 端口及对应 TLS 配置（取每端口第一个站点的证书）
fn collect_h3_ports(cfg: &AppConfig) -> Vec<(u16, &crate::config::model::TlsConfig)> {
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

/// 收集每个 TLS 端口绑定的站点名列表（热重载 diff 用）
fn collect_port_sites(cfg: &AppConfig) -> HashMap<u16, Vec<String>> {
    let mut map: HashMap<u16, Vec<String>> = HashMap::new();
    for site in &cfg.sites {
        if site.tls.is_none() { continue; }
        for &p in &site.listen_tls {
            map.entry(p).or_default().push(site.name.clone());
        }
    }
    map
}
