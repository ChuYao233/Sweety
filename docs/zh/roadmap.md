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
| PHP FastCGI | Unix Socket / TCP 连接池，等价 Nginx fastcgi_pass |
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

---

## 待实现 / 优化 🔧

### 高优先级（影响生产可用性）

| 功能 | Nginx 对应 | 说明 |
|------|-----------|------|
| **响应 Trailer 头透传** | 支持 | HTTP/1.1 chunked trailer、HTTP/2 trailer 头未透传 |

### 中优先级（协议兼容性）

| 功能 | Nginx 对应 | 说明 |
|------|-----------|------|
| **304 响应体强制为空** | 自动处理 | 上游返回 304 时响应体应为空，部分场景未验证 |
| **Accept-Encoding 协商完整性** | 完整 | gzip/br 条件压缩的 mime type 白名单需与 Nginx 对齐 |
| **TLS session ticket 复用** | 支持 | 依赖 rustls 默认行为，未显式配置 session cache 大小 |
| **HTTP/1.0 支持** | 完整 | 部分 HTTP/1.0 客户端行为（Connection: close 等）未充分验证 |

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

## 性能差距

| 项目 | Sweety | Nginx | 差距评估 |
|------|--------|-------|---------|
| 上游 HTTP 连接池 | ✅ `conn_pool` | ✅ keepalive 指令 | 等价 |
| sendfile(2) 静态文件 | ✅ Linux 路径已实现 | ✅ 全路径 | 基本等价 |
| 响应写合并（writev） | ✅ 框架支持 | ✅ | 等价 |
| TLS session resumption | 依赖 rustls | 显式 session cache | 轻微劣势 |
| 工作进程 CPU 亲和 | Tokio 自动调度 | `worker_cpu_affinity` | 轻微劣势（高核数） |
| 内存占用 | 未对比 | 极低 | 待测 |

---

## 近期计划（下一版本）

1. **响应 Trailer 透传** — chunked trailer / H2 trailer 头透传给客户端
2. **TCP/UDP 四层代理** — 无协议解析的纯字节转发（`stream {}` 等价功能）
3. **`limit_req` 全局速率限流** — 跨 worker 共享的全局限流

---

*最后更新：基于当前主分支状态*
