# Sweety 架构文档

## 总体设计原则

- **单一职责**：每个模块只负责一件事，模块间通过明确的接口通信
- **异步优先**：全程 `async/await`，基于 Tokio 运行时，零阻塞
- **可热插拔**：中间件链、Handler 均为 trait 对象，可在运行时动态替换
- **零拷贝**：静态文件通过系统 sendfile/splice 传输，避免用户态缓冲
- **单文件部署**：所有功能编译进单一二进制，无外部动态库依赖

---

## 整体架构图

```
┌──────────────────────────────────────────────────────────┐
│                      sweety 进程                          │
│                                                          │
│  ┌─────────────┐   ┌──────────────┐   ┌──────────────┐  │
│  │  server 层   │   │ dispatcher 层 │   │  handler 层   │  │
│  │             │   │              │   │              │  │
│  │ HTTP/1.1    │──▶│  VHost 分发   │──▶│ static_file  │  │
│  │ HTTP/2      │   │  Location 匹配│   │ fastcgi      │  │
│  │ HTTP/3(QUIC)│   │  Rewrite 应用 │   │ websocket    │  │
│  │ TLS(Rustls) │   │              │   │ reverse_proxy│  │
│  │ ACME        │   └──────────────┘   │ error_page   │  │
│  └─────────────┘                      └──────────────┘  │
│         │                                     │          │
│         ▼                                     ▼          │
│  ┌─────────────────────────────────────────────────┐     │
│  │                  middleware 链                   │     │
│  │  access_log → rate_limit → security → cache      │     │
│  │  metrics → error_log                             │     │
│  └─────────────────────────────────────────────────┘     │
│                                                          │
│  ┌──────────────┐   ┌──────────────┐   ┌────────────┐   │
│  │  config 模块  │   │  monitor 模块 │   │ admin_api  │   │
│  │  TOML/JSON   │   │  Prometheus  │   │ HTTP + WS  │   │
│  │  热重载       │   │  慢请求分析   │   │ 动态管理    │   │
│  └──────────────┘   └──────────────┘   └────────────┘   │
└──────────────────────────────────────────────────────────┘
```

---

## 模块详细说明

### 1. `server` — 核心服务器层

**职责**：网络监听、TLS 握手、协议升级、连接生命周期管理

子模块：

| 文件 | 说明 |
|---|---|
| `http.rs` | Xitca-Web 应用构建、监听端口绑定、请求分发入口 |
| `tls.rs` | Rustls 配置构建、证书加载、ACME 自动续期调度 |
| `quic.rs` | Quinn（QUIC 库）集成，HTTP/3 连接管理 |

**数据流**：

```
TCP 连接
  → TLS 握手（tls.rs）
  → 协议协商（HTTP/1.1 / HTTP/2 via ALPN，HTTP/3 via QUIC）
  → 连接交给 Xitca-Web 应用树
  → 进入 dispatcher 层
```

---

### 2. `dispatcher` — 路由分发层

**职责**：将请求按 Host → Location → Handler 三级路由

子模块：

| 文件 | 说明 |
|---|---|
| `vhost.rs` | 虚拟主机表（Host 精确 + 通配符匹配） |
| `location.rs` | 单站点内 Location 优先级匹配（前缀 > 正则 > 通配） |
| `rewrite.rs` | Rewrite 规则引擎（正则捕获组替换、标志位：last/break/redirect/permanent） |

**匹配优先级（参照 Nginx）**：

```
1. 精确匹配    location = /foo
2. 前缀优先    location ^~ /static/
3. 正则匹配    location ~ \.php$
4. 普通前缀    location /
```

---

### 3. `middleware` — 中间件层

**职责**：横切关注点，以中间件链形式包裹每次请求/响应

子模块：

| 文件 | 说明 |
|---|---|
| `access_log.rs` | 请求完成后写访问日志（JSON 或 Apache Combined 格式） |
| `error_log.rs` | 捕获 Handler 错误，写错误日志 |
| `metrics.rs` | 原子计数器：QPS、5xx 率、带宽、活跃 WebSocket 数 |
| `rate_limit.rs` | 令牌桶 + 滑动窗口，支持 IP/路径/Header/UA 四个维度 |
| `security.rs` | 敏感路径拦截（.git/.env 等）、注入安全响应头（CSP/HSTS 等） |
| `cache.rs` | 静态文件 ETag/Last-Modified、PHP 页面 s-maxage 注入 |

**中间件执行顺序**：

```
Request  → security → rate_limit → [handler] → cache → metrics → access_log → Response
                                       ↓ error
                                   error_log
```

---

### 4. `handler` — 请求处理器层

**职责**：针对不同类型请求的实际处理逻辑

子模块：

| 文件 | 说明 |
|---|---|
| `static_file.rs` | 零拷贝文件传输、Range 支持、默认文档、目录索引 |
| `fastcgi.rs` | FastCGI 协议实现、PHP 连接池（死连接检测 + 自动重连）、沙箱隔离 |
| `websocket.rs` | WebSocket 握手升级、帧收发、每站点独立连接注册表 |
| `reverse_proxy.rs` | 上游连接池、多种负载均衡策略、主动/被动健康检查、镜像流量 |
| `error_page.rs` | 自定义错误页面（支持配置文件指定路径或内置默认页） |

#### 反向代理负载均衡策略

| 策略 | 说明 |
|---|---|
| `round_robin` | 轮询（默认） |
| `weighted` | 加权轮询 |
| `least_conn` | 最少连接 |
| `ip_hash` | 客户端 IP 哈希 |

---

### 5. `config` — 配置管理模块

**职责**：解析、校验、分发配置，监听文件变更并热重载

子模块：

| 文件 | 说明 |
|---|---|
| `model.rs` | 所有配置结构体定义（`GlobalConfig`、`SiteConfig`、`LocationConfig` 等） |
| `loader.rs` | 多格式配置文件加载（TOML/JSON/YAML）、校验 |
| `hot_reload.rs` | 使用 `notify` 监听配置文件变更，通过 `tokio::watch` 广播新配置 |

**热重载流程**：

```
文件系统事件（inotify/FSEvents/ReadDirectoryChangesW）
  → 防抖（500ms）
  → 重新解析 + 校验
  → 通过 watch::Sender 广播 Arc<Config>
  → 各模块通过 watch::Receiver 获取最新配置引用
```

---

### 6. `monitor` — 监控与统计模块

**职责**：采集运行时指标，提供分析视图和 Prometheus 导出

子模块：

| 文件 | 说明 |
|---|---|
| `collector.rs` | 环形缓冲区存储最近 N 条请求指标（时间、路径、状态码、耗时、字节数） |
| `analyzer.rs` | 慢请求 TopN、热点路径 TopN、错误状态码分布、带宽峰值 |
| `prometheus.rs` | 将内部计数器格式化为 Prometheus text 格式（`/metrics` 接口） |

---

### 7. `admin_api` — 管理 API 模块

**职责**：提供运行时动态管理能力

子模块：

| 文件 | 说明 |
|---|---|
| `http.rs` | RESTful HTTP API（增删站点、查询统计、调整限流） |
| `websocket.rs` | WebSocket 推送实时统计流、接收管理指令 |

**API 路由规划**：

```
GET  /api/v1/sites            列出所有站点
POST /api/v1/sites            添加站点
PUT  /api/v1/sites/:name      更新站点配置
DELETE /api/v1/sites/:name    删除站点

GET  /api/v1/stats            全局统计快照
GET  /api/v1/stats/:site      单站点统计
WS   /api/v1/stats/stream     实时统计推送

GET  /api/v1/ratelimit        查看限流规则
POST /api/v1/ratelimit        添加/修改限流规则

GET  /metrics                 Prometheus 指标导出
```

---

## 并发模型

```
main thread
  └─ tokio::runtime (multi_thread, worker_threads = N)
       ├─ 监听任务（每端口一个 accept loop）
       ├─ 请求处理任务（每连接一个 task，无 thread per request）
       ├─ 热重载监听任务
       ├─ ACME 续期定时任务
       ├─ 健康检查定时任务
       └─ 管理 API 监听任务
```

共享状态通过以下方式管理：

| 数据 | 共享方式 |
|---|---|
| 站点配置 | `Arc<RwLock<SiteRegistry>>` + `tokio::watch` 热更新 |
| 限流计数器 | `Arc<DashMap<Key, TokenBucket>>` |
| 统计计数器 | `Arc<AtomicU64>` per 指标 |
| FastCGI 连接池 | `Arc<Pool<FastCgiConn>>` per 站点 |
| 反代上游池 | `Arc<UpstreamPool>` per 上游组 |

---

## 依赖库选型

| 用途 | 库 |
|---|---|
| Web 框架 | `xitca-web` |
| 异步运行时 | `tokio` |
| TLS | `rustls` + `tokio-rustls` |
| ACME | `instant-acme` |
| QUIC/HTTP3 | `quinn` + `h3` |
| 配置解析 | `serde` + `toml` + `serde_json` + `serde_yaml` |
| 正则表达式 | `regex` |
| 文件监听 | `notify` |
| 日志框架 | `tracing` + `tracing-subscriber` |
| 指标 | `prometheus` 或自研轻量计数器 |
| 并发数据结构 | `dashmap` |
| FastCGI | 自实现（参照 RFC 3875 + FastCGI 1.0 规范） |
| 国密（可选） | `libsm` 或 `openssl`（feature = "gm"） |
| CLI 参数 | `clap` |

---

## 安全设计

- **敏感文件拦截**：默认拦截 `.git/`、`.env`、`composer.json`、`.DS_Store` 等
- **HSTS**：HTTPS 站点自动注入 `Strict-Transport-Security` 头
- **CSP**：可配置 `Content-Security-Policy`
- **目录穿越防护**：路径规范化，拒绝 `../` 序列
- **FastCGI 沙箱**：每站点独立 FastCGI socket，禁止跨站点访问
- **管理 API 鉴权**：Bearer Token 或 mTLS

---

## 扩展点

1. **自定义 Handler**：实现 `Handler` trait 即可挂入路由树
2. **自定义中间件**：实现 `Middleware` trait，在配置中声明顺序
3. **国密 TLS**：通过 `feature = "gm"` 启用 SM2/SM4 替换 RSA/AES
4. **插件目录**（规划中）：动态加载 `.so` 插件，无需重编译

---

## 后续迭代规划

| 阶段 | 内容 |
|---|---|
| v0.1 | 项目骨架、配置解析、HTTP 静态文件服务可用 |
| v0.2 | TLS(Rustls) 集成、FastCGI 完整实现 |
| v0.3 | 反向代理 + 负载均衡完整实现 |
| v0.4 | ACME 自动证书、HTTP/3(QUIC) |
| v0.5 | Prometheus、管理 API 完整实现 |
| v0.6 | 国密（SM2/SM3/SM4）集成 |
| v1.0 | 生产可用，性能基准测试通过 |
