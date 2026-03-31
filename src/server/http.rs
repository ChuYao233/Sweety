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
use crate::handler::fastcgi_pool::FcgiPool;
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

        // 上游连接池：idle 连接数 = worker_connections / 128（兼顾并发与内存），keepalive_timeout 秒超时
        let pool_idle = (cfg.global.worker_connections / 128).max(8).min(256);
        let conn_pool = ConnPool::new(pool_idle, cfg.global.keepalive_timeout);

        // 第一步：收集所有端口的 TLS 配置和 SniResolver（key=端口号，热重载按端口精确更新）
        let mut port_resolvers: HashMap<u16, Arc<SniResolver>> = HashMap::new();
        let mut tls_bindings: Vec<(String, rustls::ServerConfig)> = Vec::new();
        // 注意：tls_port_set 只收集实际成功绑定的端口
        // 证书加载失败的端口不计入，防止 is_https 判断错误导致 421
        let mut tls_port_set: HashSet<u16> = HashSet::new();
        for (port, sites_for_port) in collect_tls_ports_grouped(&cfg) {
            let addr = format!("0.0.0.0:{}", port);
            let site_refs: Vec<&crate::config::model::SiteConfig> =
                sites_for_port.iter().map(|s| s.as_ref()).collect();
            match TlsManager::build_sni_server_config(&site_refs) {
                Ok((rustls_cfg, resolver)) => {
                    tls_port_set.insert(port); // 只有成功绑定的端口才计入
                    port_resolvers.insert(port, resolver);
                    tls_bindings.push((addr, rustls_cfg));
                    info!("HTTPS/HTTP2 监听: 0.0.0.0:{} ({} 个站点/证书)", port, site_refs.len());
                }
                Err(e) => {
                    // error 级别：证书加载失败必须让用户看到，打印完整错误链
                    tracing::error!(
                        "TLS 证书加载失败（端口 {}），该端口 HTTPS 不可用: {:#}",
                        port, e
                    );
                    tracing::error!(
                        "  提示：运行 `sweety -t` 可验证证书格式是否正确"
                    );
                }
            }
        }

        // HTTP 端口集合（用于判断请求是否是 HTTP）
        let http_port_set: HashSet<u16> = collect_http_ports(&cfg).into_iter().collect();

        // 为各站点创建 access_logger / proxy_cache / fcgi_cache 并直接注入 SiteInfo
        // 消除每请求 HashMap 字符串哈希查找（从 O(1) 哈希 → 直接指针解引用）
        // 注意：有日志配置的站点数量，用于判断是否需要记录 req_start
        let mut any_access_log = false;
        for site in &cfg.sites {
            // 访问日志
            let access_logger: Option<Arc<AccessLogger>> = if let Some(log_path) = &site.access_log {
                any_access_log = true;
                let fmt = match &site.access_log_format {
                    Some(f) => LogFormat::from_str(f),
                    None    => LogFormat::Combined,
                };
                match AccessLogger::file_sync(log_path, fmt) {
                    Ok(l) => {
                        info!("站点 '{}' 访问日志: {}", site.name, log_path.display());
                        Some(Arc::new(l))
                    }
                    Err(e) => {
                        tracing::warn!("站点 '{}' 访问日志初始化失败: {}", site.name, e);
                        None
                    }
                }
            } else { None };

            // FastCGI 缓存
            let fcgi_cache_arc: Option<Arc<ProxyCache>> = site.fastcgi.as_ref()
                .and_then(|fcgi| fcgi.cache.as_ref())
                .map(|cache_cfg| {
                    let proxy_cfg = crate::config::model::ProxyCacheConfig {
                        path: cache_cfg.path.clone(),
                        max_entries: cache_cfg.max_entries,
                        ttl: cache_cfg.ttl,
                        cacheable_statuses: cache_cfg.cacheable_statuses.clone(),
                        cacheable_methods: cache_cfg.cacheable_methods.clone(),
                        bypass_headers: cache_cfg.bypass_headers.clone(),
                    };
                    info!("站点 '{}' FastCGI 缓存已开启（max_entries={}, ttl={}s)",
                        site.name, cache_cfg.max_entries, cache_cfg.ttl);
                    ProxyCache::from_config(&proxy_cfg)
                });

            // 反代缓存
            let proxy_cache_arc: Option<Arc<ProxyCache>> = site.proxy_cache.as_ref()
                .map(|cache_cfg| {
                    info!("站点 '{}' 反代缓存已开启（max_entries={}, ttl={}s）",
                        site.name, cache_cfg.max_entries, cache_cfg.ttl);
                    ProxyCache::from_config(cache_cfg)
                });

            // 将三个资源注入 SiteInfo（通过注册表 upsert 更新已有条目）
            registry.inject_site_resources(
                &site.name, access_logger, proxy_cache_arc, fcgi_cache_arc,
            );
        }

        // 第二步：构建 state
        let state = AppState {
            registry: registry.clone(),
            metrics: metrics.clone(),
            cfg: cfg.clone(),
            tls_ports: Arc::new(tls_port_set),
            http_ports: Arc::new(http_port_set),
            conn_pool,
            sni_resolvers: Arc::new(port_resolvers),
            any_access_log,
            active_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            max_connections: cfg.global.max_connections,
            max_body_bytes: (cfg.global.client_max_body_size as u64) * 1024 * 1024,
            fcgi_pool: Arc::new(FcgiPool::new(
                32,   // 每地址最多 32 个 idle 连接
                cfg.global.fastcgi_connect_timeout,
                cfg.global.fastcgi_read_timeout,
            )),
        };

        // 第三步：构建 xitca-web App
        // 注册所有 HTTP 方法，避免 PUT/DELETE/PATCH/HEAD 等返回 405
        let h = || handler_service(multi_site_handler);
        let all_methods = get(h()).post(h()).put(h()).delete(h())
            .patch(h()).head(h()).options(h()).trace(h());
        let app = App::new()
            .with_state(state.clone())
            .at("/*path", all_methods)
            .at("/", get(h()).post(h()).put(h()).delete(h()).patch(h()).head(h()).options(h()).trace(h()));

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

        // 启动文件缓存 notify 监听：文件修改时自动淘汰内存缓存，无需每请求 stat
        {
            let roots: Vec<std::path::PathBuf> = cfg.sites.iter()
                .filter_map(|s| s.root.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .filter(|p| p.exists())
                .collect();
            if let Some(watcher) = crate::handler::static_file::start_file_cache_watcher(roots) {
                // watcher 必须保持存活，用 Box::leak 绑定到进程生命周期
                Box::leak(Box::new(watcher));
            }
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
/// 字段顺序按热路径访问频率排列，提升缓存行命中率
#[derive(Clone)]
pub struct AppState {
    /// 虚拟主机注册表（每请求必访）
    pub registry: Arc<VHostRegistry>,
    /// 全局指标（每请求必访）
    pub metrics: Arc<GlobalMetrics>,
    /// 应用配置（每请求必访）
    pub cfg: Arc<AppConfig>,
    /// HTTPS/TLS 端口集合（每请求必访）
    pub tls_ports: Arc<HashSet<u16>>,
    /// HTTP 明文端口集合（O(1) 查找）
    pub http_ports: Arc<HashSet<u16>>,
    /// 最大并发连接数（0 = 不限制）
    pub max_connections: usize,
    /// 最大请求体字节数（预计算，避免每请求乘法；0 = 不限制）
    pub max_body_bytes: u64,
    /// 当前活跃连接数（原子计数器，无锁）
    pub active_connections: Arc<std::sync::atomic::AtomicUsize>,
    /// 是否有任意站点配置了访问日志（用于 req_start 延迟初始化判断）
    pub any_access_log: bool,
    /// 上游 TCP/TLS 连接池（跨请求复用 idle 连接）
    pub conn_pool: ConnPool,
    /// SNI 证书 Resolver 按端口索引（热重载时原地更新证书，不断连）
    pub sni_resolvers: Arc<HashMap<u16, Arc<SniResolver>>>,
    /// FastCGI 连接池（复用 PHP-FPM 连接）
    pub fcgi_pool: Arc<FcgiPool>,
}

/// max_connections RAII 守卫：Drop 时自动减计数器
struct ConnGuard(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 多站点请求分发处理器
///
/// 参数必须是 `&WebContext` 引用（xitca-web handler_service FromRequest 约束）
async fn multi_site_handler(ctx: &WebContext<'_, AppState>) -> WebResponse {
    use std::sync::atomic::Ordering;
    let state = ctx.state();
    // req_start 延迟初始化：只有当该站点配置了访问日志时才计时
    // 大多数 bench 站点没有日志，就完全跳过此系统调用
    // req_start 延迟初始化：只有配置了访问日志的实例才计时
    let req_start: Option<std::time::Instant> = if state.any_access_log {
        Some(std::time::Instant::now())
    } else {
        None
    };

    // max_connections 限流：超出并发上限时返回 503（与 Nginx limit_conn 行为一致）
    // 限流关闭时跳过全部原子操作，除去 bench 场景下的无效开销
    let _conn_guard: Option<ConnGuard> = if state.max_connections > 0 {
        let cur = state.active_connections.fetch_add(1, Ordering::Relaxed);
        if cur >= state.max_connections {
            state.active_connections.fetch_sub(1, Ordering::Relaxed);
            state.metrics.record_status(503);
            return make_error_resp(StatusCode::SERVICE_UNAVAILABLE);
        }
        Some(ConnGuard(state.active_connections.clone()))
    } else {
        None
    };

    state.metrics.inc_requests();

    // ACME HTTP-01 challenge 响应（优先于所有站点匹配）
    // 快速路径：先检查路径是否以 "/.w" 开头，平均 99.99% 请求一次字符比较就跳过
    {
        let path = ctx.req().uri().path();
        if path.len() > 25 && path.as_bytes().get(1) == Some(&b'.')
            && path.starts_with("/.well-known/acme-challenge/")
        {
            if let Some(token) = path.get(28..) {
                if let Some(entry) = crate::server::tls::ACME_HTTP01_TOKENS.get(token) {
                    let body = entry.value().clone();
                    let mut resp = WebResponse::new(ResponseBody::from(body));
                    *resp.status_mut() = StatusCode::OK;
                    resp.headers_mut().insert(
                        CONTENT_TYPE,
                        HeaderValue::from_static("text/plain"),
                    );
                    return resp;
                }
            }
        }
    }

    // 一次解析 Host 头，同时得到 host 和 port（避免两次 split 迭代）
    // HTTP/2 下没有 Host 头，:authority 伪头被 xitca-web 放到 URI authority 里
    // 优先用 URI authority（H2/H3），回退到 Host 头（H1）
    let host_raw = ctx.req().uri().authority()
        .map(|a| a.as_str())
        .or_else(|| ctx.req().headers().get("host").and_then(|v| v.to_str().ok()))
        .unwrap_or("");
    let (host, host_port): (&str, Option<u16>) = if host_raw.starts_with('[') {
        // IPv6 格式：[::1]:8080
        if let Some(end) = host_raw.find(']') {
            let h = &host_raw[..=end];
            let p = host_raw[end + 1..].strip_prefix(':').and_then(|s| s.parse().ok());
            (h, p)
        } else {
            (host_raw, None)
        }
    } else if let Some((h, p)) = host_raw.rsplit_once(':') {
        // 普通 host:port，rsplit_once 一次拿到两部分
        (h, p.parse().ok())
    } else {
        (host_raw, None)
    };

    let path = ctx.req().uri().path();
    let request_uri = ctx.req().uri().path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(path);
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
    // host 已在上方解析为无端口字符串，直接用 lookup_by_host 跳过 strip_port 重复扫描
    let site = if is_https {
        match state.registry.lookup_by_host_strict(host) {
            Some(s) => s,
            None => {
                state.metrics.record_status(421);
                return make_error_resp(StatusCode::MISDIRECTED_REQUEST);
            }
        }
    } else {
        match state.registry.lookup_by_host(host) {
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
            let host_for_redirect = if tls_port == 443 { host.to_string() }
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

    let compiled_loc = match crate::dispatcher::location::match_location(&site.locations, effective_path) {
        Some(loc) => loc,
        None => {
            state.metrics.record_status(404);
            return make_error_resp(StatusCode::NOT_FOUND);
        }
    };
    let location = &compiled_loc.config;

    // 请求体大小限制（Content-Length 超过 client_max_body_size 时拒绝）
    let max_body_bytes = state.max_body_bytes;
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

    // return_body：直接返回文本内容体（等价 Caddy respond / Nginx return 200 "text"）
    if let Some(ref body_text) = location.return_body {
        let code = location.return_code.unwrap_or(200);
        let status = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
        let ct = location.return_content_type.as_deref()
            .unwrap_or("text/plain; charset=utf-8");
        state.metrics.record_status(code);
        let mut resp = WebResponse::new(ResponseBody::from(body_text.clone()));
        *resp.status_mut() = status;
        resp.headers_mut().insert(
            CONTENT_TYPE,
            HeaderValue::from_str(ct).unwrap_or_else(|_| HeaderValue::from_static("text/plain; charset=utf-8")),
        );
        return resp;
    }

    // return_code：直接返回状态码（健康检查）
    if let Some(code) = location.return_code {
        let status = StatusCode::from_u16(code).unwrap_or(StatusCode::OK);
        state.metrics.record_status(code);
        let mut resp = WebResponse::new(ResponseBody::empty());
        *resp.status_mut() = status;
        return resp;
    }

    // per-location limit_conn：并发连接数限制（等价 Nginx limit_conn）
    let _loc_conn_guard: Option<ConnGuard> = if compiled_loc.limit_conn > 0 {
        let cur = compiled_loc.conn_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if cur >= compiled_loc.limit_conn {
            compiled_loc.conn_count.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
            state.metrics.record_status(503);
            return make_error_resp(StatusCode::SERVICE_UNAVAILABLE);
        }
        Some(ConnGuard(Arc::clone(&compiled_loc.conn_count)))
    } else {
        None
    };

    // auth_request 前置鉴权（等价 Nginx auth_request）
    // 直接传 HeaderMap，零中间 Vec 堆分配
    if let Some(ref auth_url) = location.auth_request {
        let client_ip_for_auth = ctx.req().body().socket_addr().ip().to_string();
        match crate::handler::auth_request::check(
            auth_url,
            ctx.req().headers(),
            &client_ip_for_auth,
            &location.auth_request_headers,
            location.auth_failure_status,
        ).await {
            crate::handler::auth_request::AuthResult::Allow(_auth_headers) => {
                // 鉴权通过，继续处理（auth_headers 可在此处注入到请求，当前忽略）
            }
            crate::handler::auth_request::AuthResult::Deny(code) => {
                state.metrics.record_status(code);
                return make_error_resp(
                    StatusCode::from_u16(code).unwrap_or(StatusCode::UNAUTHORIZED)
                );
            }
        }
    }

    // 插件 on_request 前置拦截（handler = "plugin:xxx" 时短路）
    if let HandlerType::Plugin(ref plugin_name) = location.handler {
        let method_str = ctx.req().method().as_str();
        let body_len = ctx.req().headers()
            .get(xitca_web::http::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let client_ip_str2 = ctx.req().body().socket_addr().ip().to_string();
        let preq = crate::handler::plugin::PluginRequest {
            method:    method_str,
            path:      effective_path,
            headers:   ctx.req().headers(),
            client_ip: &client_ip_str2,
            body_len,
        };
        if let Some(short_resp) = crate::handler::plugin::run_plugin_request(plugin_name, &preq) {
            state.metrics.record_status(short_resp.status().as_u16());
            return short_resp;
        }
    }

    // 根据 handler 类型分发
    // FastCGI 的 try_files：先解析文件存在性，再决定走 FastCGI 还是 static
    let mut resp = match location.handler {
        HandlerType::Static => {
            crate::handler::static_file::handle_xitca(ctx, &site, &location).await
        }
        HandlerType::Fastcgi => {
            // 如果配置了 try_files，先解析目标路径再决定 handler
            if !location.try_files.is_empty() {
                let root = location.root.as_ref().or(site.root.as_ref());
                use crate::handler::static_file::TryFilesResult;
                match crate::handler::static_file::try_files_resolve(
                    &location.try_files, &path, root
                ).await {
                    // 找到文件：根据扩展名分流（.php → FastCGI，其他 → 静态）
                    TryFilesResult::File(p) => {
                        if p.extension().and_then(|e| e.to_str()) == Some("php") {
                            crate::handler::fastcgi::handle_xitca(ctx, &site, &location, Some(&p)).await
                        } else {
                            let mut static_loc = location.clone();
                            static_loc.handler = HandlerType::Static;
                            crate::handler::static_file::handle_xitca(ctx, &site, &static_loc).await
                        }
                    }
                    // =CODE（如 =404）
                    TryFilesResult::Code(code) => {
                        state.metrics.record_status(code);
                        make_error_resp(StatusCode::from_u16(code).unwrap_or(StatusCode::NOT_FOUND))
                    }
                    // 所有路径不存在，fallback 到 FastCGI（与 Nginx 行为一致）
                    TryFilesResult::NotFound => {
                        crate::handler::fastcgi::handle_xitca(ctx, &site, &location, None).await
                    }
                }
            } else {
                crate::handler::fastcgi::handle_xitca(ctx, &site, &location, None).await
            }
        }
        HandlerType::Websocket => {
            crate::handler::websocket::handle_xitca(ctx, &location).await
        }
        HandlerType::ReverseProxy => {
            crate::handler::reverse_proxy::handle_xitca(ctx, &site, &location).await
        }
        HandlerType::Grpc => {
            crate::handler::grpc::handle_xitca(ctx, &site, &location).await
        }
        // Plugin handler：on_request 已在上方处理，这里 on_request 返回 Continue
        // 插件可以作为独立 handler（不走其他 handler），直接返回 200 或交由 on_response 改写
        HandlerType::Plugin(ref plugin_name) => {
            use xitca_web::body::ResponseBody;
            let mut r = WebResponse::new(ResponseBody::none());
            *r.status_mut() = StatusCode::OK;
            // 调用 on_response：插件可在此修改响应体/头
            crate::handler::plugin::run_plugin_response(plugin_name, r)
        }
    };

    // 插件 on_response 后置处理（非 Plugin handler 也可挂载）
    if let HandlerType::Plugin(ref plugin_name) = location.handler {
        resp = crate::handler::plugin::run_plugin_response(plugin_name, resp);
    }

    state.metrics.record_status(resp.status().as_u16());

    // error_page：自定义错误页（等价 Nginx error_page 404 /404.html）
    // 使用 tokio::fs::read 异步读取，不阻塞 worker thread
    // 大多数站点没有配置 error_pages，提前短路避免状态码判断开销
    let status_u16 = resp.status().as_u16();
    if !site.error_pages.is_empty() && (400..600).contains(&status_u16) {
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

    // 注入 HSTS 响应头（仅当 HTTPS 且站点配置了 hsts_header_value 时）
    // hsts_header_value 为 None 时直接短路，零开销
    if site.hsts_header_value.is_some() && (is_https
        || host_port.map(|p| state.tls_ports.contains(&p)).unwrap_or(false))
    {
        if let Some(hsts_val) = &site.hsts_header_value {
            resp.headers_mut().insert(
                xitca_web::http::header::HeaderName::from_static("strict-transport-security"),
                hsts_val.clone(),  // HeaderValue::clone 只增引用计数，零堆分配
            );
        }
    }

    // 访问日志：非阻塞投递到 channel，后台 task 批量写文件
    // site.access_logger 直接持有，零 HashMap 查找开销
    if let Some(logger) = &site.access_logger {
        let duration_ms = req_start.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
        logger.send(AccessLogEntry {
            client_ip: ctx.req().body().socket_addr().ip().to_string(),
            method:    ctx.req().method().as_str().to_string(),
            uri:       request_uri.to_string(),
            http_version: match ctx.req().version() {
                xitca_web::http::Version::HTTP_11 => "HTTP/1.1",
                xitca_web::http::Version::HTTP_2  => "HTTP/2.0",
                xitca_web::http::Version::HTTP_3  => "HTTP/3.0",
                xitca_web::http::Version::HTTP_10 => "HTTP/1.0",
                _                                  => "HTTP/?",
            }.to_string(),
            status:    resp.status().as_u16(),
            bytes_sent: resp.headers()
                .get(xitca_web::http::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(0u64),
            referer: ctx.req().headers()
                .get(xitca_web::http::header::REFERER)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
                .to_string(),
            user_agent: ctx.req().headers()
                .get(xitca_web::http::header::USER_AGENT)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
                .to_string(),
            duration_ms,
            site: site.name.clone(),
        });
    }

    resp
}


/// 构造 HTML 错误响应（不依赖 ctx）
/// 使用预构建 Bytes 缓存，clone 只增引用计数，零堆分配
#[inline(always)]
fn make_error_resp(status: StatusCode) -> WebResponse {
    let body = crate::handler::error_page::get_error_bytes(status.as_u16());
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
    // 没有配置 HTTP 端口时不自动绑 80，纯 TLS 站点直接跳过 HTTP 绑定
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
