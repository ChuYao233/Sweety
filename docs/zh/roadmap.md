# 与 Nginx 对齐计划（Roadmap）

当前 Sweety 已实现 Nginx 反向代理 + 静态文件的核心功能。本文档跟踪所有已知与 Nginx 的差距，按优先级排列。

---

## 已完成 ✅

| 功能 | 说明 |
|------|------|
| HTTP/1.1 + HTTP/2 + HTTP/3 | 同一进程同时监听 |
| WebSocket H1 Upgrade（RFC 6455） | GET + Upgrade: websocket |
| WebSocket H2 extended CONNECT（RFC 8441） | CONNECT + :protocol=websocket，自动生成 Sec-WebSocket-Key |
| TLS 多证书 SNI 自动选最优 | ECDSA / RSA / Ed25519 |
| ACME HTTP-01 自动证书 | Let's Encrypt / ZeroSSL / Buypass |
| ACME DNS-01 通配符证书 | Cloudflare / Aliyun / Shell |
| 反向代理负载均衡 | round_robin / weighted / least_conn / ip_hash |
| 上游连接池复用 | TCP keepalive 池，等价 Nginx upstream keepalive |
| 断路器 | 三状态机，等价 Nginx max_fails/fail_timeout 且更精确 |
| 主动健康检查 | HTTP 探针，间隔可配 |
| 静态文件服务 | 内存缓存 + Range + ETag/Last-Modified + try_files |
| PHP FastCGI | Unix Socket / TCP 连接池，等价 Nginx fastcgi_pass；正确处理 HTTP/2 Cookie 合并（RFC 7540 §8.1.2.5），`Set-Cookie` 头完整透传 |
| gRPC 代理 | Content-Type: application/grpc + Trailer |
| Brotli + gzip 双压缩 | 优先 br，等价 Nginx gzip + ngx_brotli 模块 |
| auth_request 子请求鉴权 | 等价 Nginx auth_request |
| Rewrite 规则引擎 | last / break / redirect / permanent + !-f / !-d 条件 |
| 五维度令牌桶限流 | IP / 路径 / IP+路径 / Header / UA |
| HSTS + force_https | 等价 Nginx add_header Strict-Transport-Security |
| sub_filter 响应体替换 | 等价 Nginx sub_filter |
| 配置热重载不断连 | 等价 nginx -s reload |
| 访问日志（combined/json/自定义） | 异步写，不占 worker |
| Admin REST API | 健康检查 / 统计 / 热重载 / 节点管理 |
| 插件系统 | Rust trait，运行时注册 |
| **Expect: 100-continue 处理** | RFC 7231 §5.1.1，发头等上游回 100 再 pipe body，上游拒绝则直接返回 |
| **chunked 请求体流式透传** | `RequestBody` stream 直接 pipe 给上游，零内存拷贝，不全量 collect |
| **proxy_read_timeout 逐包语义** | 响应体流式 spawn task 每次 `read()` 独立超时，等价 Nginx 两包间隔超时语义 |
| **SO_REUSEPORT 多核扩展** | 每个 worker 线程独立 bind 同一端口，内核连接负载均衡，无锁竞争，对标 Nginx `worker_processes` |
| **H2 per-connection writer loop** | 每连接单独 writer task，HEADERS 优先 + round-robin DATA 调度，消除 head-of-line blocking，下载不饿死小请求 |
| **ACME 自签名占位启动** | 证书文件不存在时生成自签名占位，443 端口始终 bind，申请成功后热重载，对标 Caddy |
| **304 响应体强制为空** | 静态文件、反向代理、FastCGI 三路均已实现 `ResponseBody::none()`，完全符合 RFC 7230 |
| **chunked trailer 头透传** | 上游 HTTP/1.1 chunked trailer 已收集并 append 到响应（RFC 7230 §4.1.2） |
| **TLS session cache** | rustls `ServerSessionMemoryCache::new(65536)`，高并发大量客户端复用 TLS session，避免重复握手 |
| **HTTP/1.0 Connection: close** | 上游响应 `Connection: close` 时不将连接归还池，正确处理 HTTP/1.0 语义 |

---

## 待实现 / 优化 🔧

### 高优先级

| 功能 | Nginx 对应 | 说明 |
|------|-----------|------|
| **zero-copy sendfile 大文件** | `sendfile on` | 大文件下载绕过用户态 buffer，直接 DMA 传输，降低 CPU 占用 |

### 中优先级（协议兼容性）

| 功能 | Nginx 对应 | 说明 |
|------|-----------|------|
| **Accept-Encoding 协商完整性** | 完整 | gzip/br 条件压缩的 mime type 白名单需与 Nginx 完整对齐 |

### 低优先级（高级功能）

| 功能 | Nginx 对应 | 说明 |
|------|-----------|------|
| TCP/UDP 四层代理 | `stream {}` 模块 | 无协议解析的纯字节转发 |
| `if` 条件块 / `map` 变量 | Nginx `if` / `map` | 配置层的条件逻辑 |
| `geo` 模块 | IP 地理位置路由 | 基于 IP 段的配置分发 |
| `slice` 大文件分片缓存 | `proxy_cache` + `slice` | 大文件 range 分片缓存 |
| `mirror` 请求镜像 | `mirror` 指令 | 流量复制到镜像上游（灰度/影子测试） |
| `limit_req` 全局速率限流 | `limit_req_zone` | 跨 worker 共享的全局限流（当前是单 worker 令牌桶） |
| Prometheus 指标推送 | Nginx Plus 或 exporter | 当前有 `/metrics` 拉取，无主动推送 |

---

## 性能基准

**测试环境**：Intel J4105 四核四线程 @ 1.5GHz，2GB RAM，Debian Linux
**测试工具**：[bombardier](https://github.com/codesenberg/bombardier) `-c 1000 -d 30s -k --latencies --http2`
**测试文件**：10KB 静态二进制文件（`/10kb.bin`）
**对比版本**：Nginx 1.28.1 vs Sweety（当前主分支）

| 指标 | Nginx 1.28.1 | Sweety | 差异 |
|------|------------|--------|------|
| **RPS 平均** | 6252 | **8425** | **+35%** |
| **吸吐量** | 57.93 MB/s | **83.94 MB/s** | **+45%** |
| **延迟 P50** | 95ms | **112ms** | 相近 |
| **延迟 P75** | 193ms | **115ms** | **-40%** |
| **延迟 P90** | 315ms | **165ms** | **-48%** |
| **延迟 P95** | 486ms | **168ms** | **-65%** |
| **延迟 P99** | 1300ms | **172ms** | **-87%** |
| **延迟 Stdev** | 153ms | **6ms** | 极其平稳 |
| **错误数** | 19594 | **0** | 零错误 |
| **2xx 请求数** | 168878 | **253604** | +50% |

> 尾延迟（P95/P99）是高并发场景下最关键的用户体验指标。Nginx 在 1000 并发连接下 P99=1.3s 且有近 2 万错误（GOAWAY 导致的连接重置）；Sweety P99=172ms、零错误、标准差仅 6ms。

| 项目 | Sweety | Nginx | 差距评估 |
|------|--------|-------|------|
| 上游 HTTP 连接池 | ✅ `conn_pool` | ✅ keepalive 指令 | 等价 |
| sendfile(2) 静态文件 | ✅ Linux 路径已实现 | ✅ 全路径 | 基本等价 |
| 响应写合并（writev） | ✅ 框架支持 | ✅ | 等价 |
| 多核扩展 | ✅ SO_REUSEPORT per-worker | ✅ worker_processes | 等价 |
| TLS session resumption | ✅ rustls cache 65536 | ✅ 显式 session cache | 等价 |
| H2 HOL blocking | ✅ per-connection writer loop | ✅ | 等价 |
| 工作进程 CPU 亲和 | Tokio 自动调度 | `worker_cpu_affinity` | 轻微劣势（高核数） |
| zero-copy sendfile 大文件 | 待实现 | ✅ | 待补齐 |

---

## 近期计划（下一版本）

1. **zero-copy sendfile 大文件** — 大文件绕过用户态 buffer 直接 DMA 传输，降低 CPU 占用
2. **`limit_req` 全局速率限流** — 跨 worker 共享的全局限流
3. **TCP/UDP 四层代理** — 无协议解析的纯字节转发（`stream {}` 等价功能）

---

*最后更新：2026-04-01，基于当前主分支状态，测试数据：bombardier 1000c 30s HTTP/2 10KB.bin，J4105 2GB RAM*
