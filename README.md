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
| **TLS** | Rustls + ACME 自动证书（Let's Encrypt） |
| **多站点** | 虚拟主机（Host 匹配）、单站点多域名、站点隔离 |
| **静态文件** | 零拷贝 sendfile、默认文档（index.html）、目录浏览可选 |
| **PHP/FastCGI** | 高并发连接池、沙箱隔离，与 Nginx 相同实现方式 |
| **WebSocket** | 高并发、多站点独立连接管理 |
| **反向代理** | 负载均衡（轮询/加权/最小连接）、健康检查、请求镜像 |
| **Rewrite/伪静态** | 前缀匹配、正则重写、301/302 跳转 |
| **限流** | 按 IP / 路径 / Header / User-Agent 多维度令牌桶限流 |
| **安全** | 敏感文件拦截、自动安全响应头 |
| **缓存** | 静态文件 ETag/Last-Modified、PHP 页面可选缓存 |
| **日志** | 访问日志（JSON/文本双模式）、错误日志、日志轮转 |
| **监控** | 实时统计（QPS、带宽、WebSocket 连接数）、Prometheus 导出 |
| **管理 API** | HTTP + WebSocket 双协议，动态增删站点、调整限流等 |
| **热重载** | 配置文件修改后无需重启，实时生效 |
| **部署** | 单文件可执行，轻量可移植 |

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
- [ ] HTTP/1.1 完整静态文件服务
- [ ] TLS（Rustls）集成
- [ ] FastCGI 连接池完整实现
- [ ] WebSocket 高并发完整实现
- [ ] 反向代理负载均衡完整实现
- [ ] ACME 自动证书
- [ ] HTTP/3（QUIC）集成
- [ ] Prometheus 导出完整实现
- [ ] 管理 WebSocket API 完整实现

---

## License

MIT
