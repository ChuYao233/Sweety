# Sweety

> 高性能、单文件部署、多站点 Web 服务器 —— Rust + Xitca-Web 构建

---

## 简介

Sweety 是一款以 Rust 编写、基于 [Xitca-Web](https://github.com/HFQR/xitca-web) 异步运行时的高性能 Web 服务器，
目标是以单一可执行文件提供媲美 Nginx / Caddy 的多站点服务能力，同时具备现代云原生特性。

---

## 功能特性

| 功能分类 | 支持内容 |
|---|---|
| **协议** | HTTP/1.1、HTTP/2、HTTP/3（QUIC） |
| **TLS** | Rustls 纯 Rust TLS、多证书（ECDSA + RSA + Ed25519）、ACME 自动证书（Let's Encrypt / ZeroSSL / Buypass）、TLS 版本控制 |
| **多站点** | 虚拟主机 SNI 隔离、HTTPS 跨站防护（421）、`fallback` 兜底站点 |
| **静态文件** | 流式传输（ReaderStream）、Range 分块、**Brotli + gzip** 双压缩（优先 br）、ETag/Last-Modified 缓存、`try_files` |
| **PHP/FastCGI** | 高并发连接池、死连接自动剔除、`fastcgi_cache`（内存+磁盘双层） |
| **WebSocket** | 高并发 WS/WSS 反向代理、站点级 `websocket` 开关 |
| **反向代理** | 负载均衡（轮询/加权/最少连接/IP哈希）、主动健康检查、连接池复用、`proxy_cache`（内存+磁盘双层） |
| **gRPC 代理** | `handler = "grpc"`，自动注入 `Content-Type: application/grpc`，处理 `grpc-status` Trailer |
| **Rewrite/伪静态** | 前缀/正则/精确匹配、Rewrite 规则（条件触发）、301/302 跳转 |
| **限流** | 按 IP / 路径 / IP+路径 / Header / User-Agent 五维度令牌桶；`nodelay` 模式（等价 Nginx limit_req nodelay） |
| **鉴权** | `auth_request` 子请求鉴权（等价 Nginx auth_request），支持完整 URL 或相对路径 |
| **安全** | 敏感文件拦截、HTTPS 跨站隔离、请求体大小限制（413）、HSTS、`force_https` |
| **响应处理** | `sub_filter` 内容替换（字符串/正则）、`add_headers` 注入、`proxy_set_headers` 覆盖、`cache_rules` 按扩展名缓存、`return_body` 直接返回内容体 |
| **缓存** | 静态文件 ETag/Last-Modified、`proxy_cache`、`fastcgi_cache`、按扩展名 `Cache-Control` |
| **压缩** | Brotli（优先，压缩率高 20-30%）+ gzip（降级），全局/站点级覆盖，可配等级和最小文件大小 |
| **日志** | 访问日志异步写文件（Nginx combined / JSON / 自定义 log_format 格式）、错误日志、日志级别动态配置 |
| **监控** | 实时统计（QPS、带宽）、Prometheus 导出 |
| **管理 API** | HTTP + WebSocket 双协议，动态增删站点 |
| **热重载** | 配置/证书文件变更后自动 diff 更新，不断开现有连接 |
| **连接配置** | `worker_connections`、`keepalive_timeout`、`max_connections`、`client_max_body_size` 等 Nginx 同名配置 |
| **部署** | 单文件可执行，无 C 依赖（纯 Rust），支持 Linux / macOS / Windows |

---

## 快速开始

### 环境要求

- Rust 1.78+（`rustup update stable`）
- cargo

### 编译

```bash
cargo build --release
```

### 启动

```bash
./target/release/sweety --config config/sweety.toml
```

### 最简配置示例

```toml
# 静态网站 + 自动 HTTPS
[[sites]]
name        = "my-site"
server_name = ["example.com", "www.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/example"
force_https = true

[sites.tls]
acme       = true
acme_email = "admin@example.com"

[[sites.locations]]
path    = "/"
handler = "static"
```

完整配置参考 [`config/sweety.example.toml`](config/sweety.example.toml)。

---

## 项目结构

```
sweety/
├─ src/
│  ├─ main.rs           # 程序入口、CLI 参数解析
│  ├─ lib.rs            # 公共导出
│  ├─ server/           # 核心服务器（HTTP/TLS/QUIC 监听）
│  ├─ dispatcher/       # 路由分发（虚拟主机、Location、Rewrite）
│  ├─ middleware/        # 中间件（日志、限流、安全、缓存、统计）
│  ├─ handler/          # 请求处理器
│  │  ├─ static_file.rs # 静态文件（Brotli/gzip/Range）
│  │  ├─ fastcgi.rs     # PHP/FastCGI（流式响应、连接池）
│  │  ├─ reverse_proxy/ # 反向代理（HTTP/WS/负载均衡）
│  │  ├─ grpc.rs        # gRPC 反向代理
│  │  ├─ auth_request.rs# 子请求鉴权（auth_request）
│  │  └─ websocket.rs   # WebSocket 代理
│  ├─ config/           # 配置加载与热重载
│  ├─ monitor/          # 监控收集、Prometheus
│  └─ admin_api/        # 管理 API（HTTP + WebSocket）
├─ config/
│  ├─ sweety.example.toml  # 完整配置示例（含所有配置项）
│  └─ sweety.toml          # 实际运行配置
└─ docs/
   └─ architecture.md   # 详细架构文档
```

---

## 模块说明

| 模块 | 职责 |
|---|---|
| `server` | 监听端口、TLS 握手、协议升级、ACME 自动证书、连接生命周期管理 |
| `dispatcher` | 按 Host 选站点、按 Location 路径分发、Rewrite 规则应用 |
| `middleware` | 访问日志、限流（令牌桶）、安全头、ETag 缓存、Prometheus 指标 |
| `handler` | 静态文件、FastCGI、gRPC、WebSocket、反向代理、auth_request、错误页 |
| `config` | TOML/JSON/YAML 解析、配置结构体定义、热重载监听（inotify/kqueue/FSEvent） |
| `monitor` | 指标采集、热点路径统计、Prometheus 接口 |
| `admin_api` | 运行时管理接口，支持动态修改站点配置、限流规则等 |

---

## 配置格式

支持三种格式，通过文件扩展名自动识别：

| 格式 | 扩展名 | 适用场景 |
|---|---|---|
| **TOML** | `.toml` | 推荐，人类可读性最佳 |
| **JSON** | `.json` | 程序生成、API 下发 |
| **YAML** | `.yaml` / `.yml` | CI/CD 流水线、Kubernetes ConfigMap |

### 配置项速查

| 配置块 | 关键字段 |
|---|---|
| `[global]` | `worker_threads`、`worker_connections`、`max_connections`、`keepalive_timeout`、`fastcgi_connect_timeout`、`fastcgi_read_timeout`、`client_max_body_size`、`gzip`、`gzip_comp_level`、`gzip_min_length`、`admin_listen`、`admin_token`、`prometheus_enabled`、`log_level` |
| `[[sites]]` | `name`、`server_name`、`listen`、`listen_tls`、`root`、`index`、`access_log`、`error_log`、`force_https`、`fallback`、`gzip`、`gzip_comp_level`、`websocket` |
| `[sites.tls]` | `acme`、`acme_email`、`acme_provider`、`acme_renew_days_before`、`cert`、`key`、`certs[]`、`min_version`、`max_version` |
| `[sites.hsts]` | `max_age`、`include_sub_domains`、`preload` |
| `[sites.fastcgi]` | `socket`、`host`、`port`、`pool_size`、`connect_timeout`、`read_timeout` |
| `[sites.fastcgi.cache]` | `path`、`max_entries`、`ttl`、`cacheable_statuses`、`cacheable_methods`、`bypass_headers` |
| `[sites.proxy_cache]` | 同 `fastcgi.cache` |
| `[[sites.upstreams]]` | `name`、`strategy`、`health_check`、`nodes[]`（`addr`/`weight`/`tls`/`tls_sni`/`tls_insecure`/`upstream_host`） |
| `[[sites.rewrites]]` | `pattern`、`target`、`flag`（last/break/redirect/permanent）、`condition`（`!-f`/`!-d`） |
| `[sites.rate_limit]` | `rules[]`：`dimension`（ip/path/ip_path/header/user_agent）、`rate`、`burst`、`nodelay`、`path_pattern`、`header_name` |
| `[[sites.locations]]` | `path`、`handler`（static/fastcgi/reverse_proxy/grpc/websocket）、`upstream`、`root`、`try_files`、`return_code`、`return_url`、`cache_control`、`auth_request`、`auth_failure_status`、`auth_request_headers[]`、`proxy_set_headers[]`、`add_headers[]`、`sub_filter[]`、`cache_rules[]`、`strip_cookie_secure`、`proxy_cookie_domain`、`proxy_redirect_from`/`_to`、`max_connections` |

---

## 与 Nginx / Caddy 对比

| 功能 | Sweety | Nginx | Caddy |
|---|---|---|---|
| HTTP/3 内置 | ✅ | ❌ 需重新编译 | ✅ |
| ACME 自动证书 | ✅ | ❌ 需 certbot | ✅ 零配置 |
| Brotli 压缩 | ✅ | ❌ 需第三方模块 | ✅ |
| FastCGI 缓存 | ✅ | ✅ | ✅ |
| auth_request | ✅ | ✅ | ❌ 需插件 |
| gRPC 代理 | ✅ | ✅ | ✅ |
| WebSocket 代理 | ✅ | ✅ | ✅ |
| 单文件无依赖 | ✅ | ❌ | ✅ |
| 内存安全 | ✅ Rust | ❌ C | ✅ Go |
| Windows 多线程 | ✅ IOCP | ❌ 单线程 select | ⚠️ |
| TCP/UDP 四层代理 | ❌ | ✅ stream 模块 | ✅ layer4 插件 |
| Lua 脚本扩展 | ❌ | ✅ OpenResty | ❌ |
| GC 暂停抖动 | ✅ 无 GC | ✅ 无 GC | ⚠️ Go GC |

---

## 已知问题

### `xitca_http::error` H2 `UnexpectedFrameType` 日志

运行时可能出现如下 ERROR 日志：

```
ERROR xitca_http::error: target="h2_dispatcher" self=H2(Error { kind: User(UnexpectedFrameType) })
```

**原因**：这是底层依赖 `xitca-http 0.7.1` 的 H2 dispatcher 的已知 bug。
当浏览器在 H2 多路复用下取消请求（发送 RST_STREAM 帧）时，xitca-http 的状态机没有正确处理该帧类型，
触发这条错误日志。**不影响功能**，页面、API、反代均正常工作。

**临时解决方案**：在配置文件中将 `xitca_http` 日志级别设为 `off`：

```toml
log_level = "sweety_lib=info,xitca_server=warn,xitca_web=warn,xitca_http=off"
```

等待上游 [xitca-web](https://github.com/HFQR/xitca-web) 修复后升级即可彻底解决。

---

## 路线图

- [x] HTTP/1.1 + HTTP/2 静态文件服务（流式、Range、ETag）
- [x] TLS（Rustls）——多证书 ECDSA/Ed25519/RSA、SNI Resolver、TLS 版本控制
- [x] ACME 自动证书（HTTP-01，支持 Let's Encrypt / ZeroSSL / Buypass）
- [x] HTTP/3（QUIC）集成
- [x] FastCGI 连接池（死连接剔除、超时重试、流式响应）
- [x] `fastcgi_cache`（内存+磁盘，TTL/bypass 头配置，复用 proxy_cache 实现）
- [x] WebSocket 高并发 WS/WSS 反向代理
- [x] 反向代理负载均衡（轮询/加权/最小连接/IP哈希）+ 连接池复用 + 主动健康检查
- [x] **gRPC 反向代理**（`handler = "grpc"`，grpc-status Trailer 注入）
- [x] **auth_request 子请求鉴权**（等价 Nginx auth_request，支持 TLS 鉴权端点）
- [x] Rewrite/伪静态规则（条件触发 `!-f` / `!-d`，301/302 跳转）
- [x] 限流（五维度令牌桶：IP/路径/IP+路径/Header/UA，`nodelay` 模式）
- [x] HTTPS 跨站防护（421 Misdirected Request）
- [x] `fallback` 兜底站点
- [x] 配置/证书热重载（diff 更新，不断连）
- [x] **Brotli 压缩**（优先于 gzip，压缩率高 20-30%）+ gzip（全局/站点级覆盖）
- [x] HSTS 响应头注入（含 includeSubDomains / preload）
- [x] 请求体大小限制（`client_max_body_size`，超限返回 413）
- [x] Nginx 同名连接配置（`worker_connections`、`keepalive_timeout`、`max_connections` 等）
- [x] Prometheus 指标导出
- [x] 管理 API（HTTP + WebSocket）
- [x] `return` 指令（带 URL，支持 `$request_uri` 变量）
- [x] `try_files`（支持 `$uri`/`$uri/`/固定路径/`=404`）
- [x] `error_page` 自定义错误页（按状态码匹配）
- [x] 访问日志异步写文件（Nginx combined / JSON / 自定义 log_format 格式）
- [x] `proxy_cache`（内存+磁盘双层，TTL/可缓存状态码/bypass 头配置）
- [x] `sub_filter`（响应体内容替换，支持字符串和正则）
- [x] `force_https`（HTTP 自动跳转 HTTPS）
- [x] `proxy_set_headers` / `add_headers` / `cache_rules` / `strip_cookie_secure`
- [x] `return_body` / `return_text` 直接返回内容体（不只是状态码）
- [x] per-location `limit_conn`（并发连接限制）
- [x] 访问日志 `log_format` 自定义格式（变量插值）
- [x] upstream `keepalive_requests` / `keepalive_time` 精细控制
- [x] `proxy_buffering` 控制（缓冲/流式两种模式）
- [x] 文件缓存 notify 自动失效（文件修改时实时淘汰内存缓存）
- [x] 错误页预构建 Bytes 缓存（零 format! 分配）
- [ ] ACME DNS-01 验证（通配符证书，支持 Cloudflare / 阿里云等 provider）
- [ ] 自动 HTTPS（零配置，detect server_name 自动申请 ACME）
- [ ] templates 模板渲染（Tera 集成）
- [ ] TCP/UDP 四层代理（stream 模块）
- [ ] 流式 gzip/brotli（大文件 > 4MB 在线压缩）
- [ ] HTTP/2 Server Push
- [ ] OnDemand TLS（多租户动态申请证书）

---

## License

MIT
