# Sweety

> 高性能、单文件部署、多站点 Web 服务器 —— Rust + Xitca-Web 构建

---

## 简介

Sweety 是一款以 Rust 编写、基于 [Xitca-Web](https://github.com/HFQR/xitca-web) 异步运行时的高性能 Web 服务器，
目标是以单一可执行文件提供媲美 Nginx 的多站点服务能力，同时具备现代云原生特性。

---

## 功能特性

| 功能分类 | 支持内容 |
|---|---|
| **协议** | HTTP/1.1、HTTP/2、HTTP/3（QUIC） |
| **TLS** | Rustls 纯 Rust TLS、多证书（ECDSA + Ed25519 等）、ACME 自动证书、TLS 版本控制、证书透明度日志（CT Log）上报 |
| **多站点** | 虚拟主机 SNI 隔离、HTTPS 跨站防护（421）、`fallback` 兜底站点 |
| **静态文件** | 流式 0-copy（ReaderStream）、Range 分块、gzip 压缩、ETag/Last-Modified 缓存、`try_files` |
| **PHP/FastCGI** | 高并发连接池、沙箱隔离，与 Nginx 相同实现方式 |
| **WebSocket** | 高并发 WS/WSS 反向代理、站点级 websocket 开关 |
| **反向代理** | 负载均衡（轮询/加权/最少连接/IP哈希）、健康检查、连接池复用、`proxy_cache`（内存+磁盘双层） |
| **Rewrite/伪静态** | 前缀匹配、正则重写、301/302 跳转 |
| **限流** | 按 IP / 路径 / Header / User-Agent 多维度令牌桶限流 |
| **安全** | 敏感文件拦截、HTTPS 跨站隔离、请求体大小限制（413）、HSTS、`force_https` HTTP→HTTPS 强制跳转 |
| **缓存** | 静态文件 ETag/Last-Modified、Cache-Control 按扩展名默认 |
| **压缩** | 全局/站点级 gzip，可配置等级和最小文件大小 |
| **日志** | 访问日志异步写文件（Nginx combined 格式）、JSON 格式、错误日志 |
| **监控** | 实时统计（QPS、带宽）、Prometheus 导出 |
| **管理 API** | HTTP + WebSocket 双协议，动态增删站点 |
| **热重载** | 配置/证书文件变更后自动 diff 更新，不断开现有连接；端口变更指引重启 |
| **连接配置** | `worker_connections`、`keepalive_timeout`、`client_max_body_size` 等 Nginx 同名配置 |
| **部署** | 单文件可执行，无 C 依赖（纯 Rust），轻量可移植 |

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
[global]
worker_threads = 4

[[sites]]
server_name = ["example.com", "www.example.com"]
listen = 80
root = "/var/www/example"
index = ["index.html", "index.htm"]
access_log = "/var/log/sweety/example_access.log"
error_log  = "/var/log/sweety/example_error.log"

[[sites.locations]]
path = "/"
handler = "static"
```

更完整示例见 [`config/sweety.example.toml`](config/sweety.example.toml)。

---

## 项目结构

```
sweety/
├─ src/
│  ├─ main.rs          # 程序入口、CLI 参数解析
│  ├─ lib.rs           # 公共导出
│  ├─ server/          # 核心服务器（HTTP/TLS/QUIC 监听）
│  ├─ dispatcher/      # 路由分发（虚拟主机、Location、Rewrite）
│  ├─ middleware/       # 中间件（日志、限流、安全、缓存、统计）
│  ├─ handler/         # 请求处理器（静态、FastCGI、WS、反代、错误页）
│  ├─ config/          # 配置加载与热重载
│  ├─ monitor/         # 监控收集、分析、Prometheus
│  └─ admin_api/       # 管理 API（HTTP + WebSocket）
├─ config/
│  └─ sweety.example.toml
├─ docs/
│  └─ architecture.md  # 详细架构文档
└─ tests/              # 集成测试
```

详细架构说明见 [`docs/architecture.md`](docs/architecture.md)。

---

## 模块说明

| 模块 | 职责 |
|---|---|
| `server` | 监听端口、TLS 握手、协议升级、连接生命周期管理 |
| `dispatcher` | 按 Host 选站点、按 Location 路径分发、Rewrite 规则应用 |
| `middleware` | 横切关注点：日志、限流、安全头、ETag 缓存 |
| `handler` | 具体请求处理：静态文件、FastCGI、WebSocket、反代、错误页 |
| `config` | 配置文件解析（TOML/JSON/YAML）、结构体定义、热重载监听 |
| `monitor` | 指标采集、慢请求分析、热点路径统计、Prometheus 接口 |
| `admin_api` | 运行时管理接口，支持动态修改站点配置、限流规则等 |

---

## 配置格式

支持三种格式，通过文件扩展名自动识别：

- `.toml` — 推荐，人类可读性最佳
- `.json` — 适合程序生成
- `.yaml` / `.yml` — 兼容 CI/CD 流水线

---

## 路线图

- [x] 项目骨架与基础模块
- [x] HTTP/1.1 + HTTP/2 静态文件服务（流式 0-copy、Range、gzip、ETag）
- [x] TLS（Rustls）集成——多证书 ECDSA/Ed25519/RSA、SNI Resolver、TLS 版本控制
- [x] ACME 自动证书（Let's Encrypt TLS-ALPN-01）
- [x] HTTP/3（QUIC）集成
- [x] FastCGI 连接池完整实现
- [x] WebSocket 高并发 WS/WSS 反向代理
- [x] 反向代理负载均衡（轮询/加权/最小连接/IP哈希）+ 连接池复用
- [x] Rewrite/伪静态规则（正则、301/302）
- [x] 限流（IP/路径/Header/UA 多维度令牌桶）
- [x] HTTPS 跨站防护（421 Misdirected Request）
- [x] Fallback 兜底站点（显式 `fallback = true`）
- [x] 配置/证书热重载（diff 更新，不断连）
- [x] gzip 压缩（全局 + 站点级覆盖，可配置等级和最小文件大小）
- [x] HSTS 响应头注入
- [x] 请求体大小限制（`client_max_body_size`，超限返回 413）
- [x] Nginx 同名连接配置（`worker_connections`、`keepalive_timeout` 等）
- [x] Prometheus 指标导出
- [x] 管理 API（HTTP + WebSocket）
- [x] `return` 指令（带 URL 的 return 301/302，支持 `$request_uri` 变量）
- [x] `try_files`（静态文件 fallback，支持 `$uri`/`$uri/`/固定路径/`=404`）
- [x] `error_page` 自定义错误页（按状态码匹配）
- [x] 访问日志异步写文件（Nginx combined 格式、JSON 格式）
- [x] `proxy_cache`（内存+磁盘双层，TTL/可缓存状态码/bypass 头配置）
- [x] `sub_filter`（反代响应体内容替换，支持字符串和正则）
- [x] `force_https`（HTTP 自动跳转 HTTPS，与 Nginx 行为一致）
- [ ] 流式 gzip（大文件在线压缩，当前 > 4MB 跳过）
- [ ] HTTP/2 Server Push
- [ ] 证书透明度日志（CT Log）上报

---

## License

MIT
