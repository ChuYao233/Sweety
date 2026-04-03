# Roadmap

> **免责声明**：Sweety 目前仍处于积极开发阶段，**尚未经过生产环境验证**。
> 不建议在关键业务场景下直接使用，欢迎在测试/开发环境试用并反馈问题。

当前 Sweety 已覆盖 Nginx 反向代理 + 静态文件的核心功能集，同时兼具 Caddy 的开箱即用体验。本文档跟踪已完成特性、在建工作与后续计划。

---

## 已完成

### 协议
- HTTP/1.1 + HTTP/2 + HTTP/3（QUIC）同一进程同时监听
- WebSocket H1 Upgrade（RFC 6455）+ H2 extended CONNECT（RFC 8441）全透传
- TLS：rustls 纯 Rust，多证书 SNI 自动路由，TLS session cache（65536 entries）
- ACME HTTP-01 自动证书（Let's Encrypt / ZeroSSL / Buypass / LiteSSL）
- ACME DNS-01 通配符证书（Cloudflare / 阿里云 / Shell 自定义）
- ACME 自签名占位启动：证书未就绪时自动生成占位证书，申请成功后热重载，对标 Caddy

### 请求处理
- 静态文件：内存 LRU 缓存 + Range + ETag/Last-Modified + try_files + sendfile(2)
- PHP/FastCGI：Unix Socket / TCP 连接池，fastcgi_cache，正确处理 HTTP/2 Cookie 合并（RFC 7540 §8.1.2.5）
- 反向代理：轮询 / 加权 / 最少连接 / IP 哈希 + 连接池 + 断路器（三状态机）+ 主动健康检查 + proxy_cache
- HTTP/2 上游支持（h2c + h2 over TLS）
- gRPC 代理：application/grpc + gRPC-Web + Trailer 透传
- auth_request 子请求鉴权
- Brotli + gzip 双压缩（优先 br）
- sub_filter 响应体内容替换
- Expect: 100-continue 正确处理（RFC 7231 §5.1.1）
- chunked 请求体流式透传（零内存拷贝）
- proxy_read_timeout 逐包语义（两包间隔超时，等价 Nginx 行为）

### 路由
- 虚拟主机：精确 / 通配符 / fallback 兜底
- Location 四级优先级：`= 精确` > `^~ 前缀优先` > `~ 正则` > `普通前缀`
- Rewrite 规则引擎：正则捕获，last / break / redirect / permanent，!-f / !-d 条件

### 配置易用性（Caddy 风格）
- `preset = "wordpress"` — 一行自动展开 WordPress 最优 location 规则
- `preset = "laravel"` — 一行自动展开 Laravel 路由规则
- `preset = "static"` — 静态站点预设
- `php_fastcgi = "/tmp/php.sock"` — 一行代替完整 `[sites.fastcgi]` 块
- `acme_email = "you@example.com"` — 一行开启 ACME 自动 HTTPS，无需写 `[sites.tls]`

### 安全与可靠性
- 断路器：三状态机（Closed → Open → Half-Open），比 Nginx max_fails 更精确
- 五维度令牌桶限流：IP / 路径 / IP+路径 / Header / User-Agent
- HSTS + force_https
- 304 响应体强制为空（RFC 7230 §3.3）

### 性能架构
- SO_REUSEPORT 多核扩展：每 worker 线程独立 bind，内核连接负载均衡
- H2 per-connection writer loop：HEADERS 优先 + round-robin DATA 调度，消除 head-of-line blocking
- 静态文件双键缓存（fast path 跳过 canonicalize/stat syscall，热路径零系统调用）

### 运维
- 配置热重载：不断开现有连接（等价 nginx -s reload）
- 访问日志：combined / json / 自定义模板，异步写
- Admin REST API：健康检查 / 统计 / 节点管理 / 热重载
- Prometheus `/metrics` 端点
- Daemon 模式：start / stop / restart / PID 文件
- 配置验证：sweety validate（等价 nginx -t）
- 多格式配置：TOML / JSON / YAML 自动识别

### 代码质量
- config/model 拆分为 global.rs / site.rs / tls.rs / location.rs / upstream.rs
- server/http.rs 拆分为 state.rs / router.rs / http.rs
- handler/static_file 拆分为 cache.rs / compress.rs / range.rs / path.rs
- handler/fastcgi 拆分为 proto.rs / response.rs

---

## 进行中

| 项目 | 说明 |
|------|------|
| 插件系统完善 | Rust trait 动态注册，完善 API 文档 |
| 全局速率限流 | 当前为单 worker 令牌桶，计划基于 `DashMap` 实现跨 worker 共享 |

---

## 计划中

### 高优先级

| 功能 | 对应 Nginx | 说明 |
|------|-----------|------|
| TCP/UDP 四层代理 | `stream {}` 模块 | 纯字节转发，无协议解析，支持数据库/SSH/任意 TCP 代理 |
| `mirror` 请求镜像 | `mirror` 指令 | 流量异步复制到镜像上游（灰度测试 / 影子流量） |

### 中优先级

| 功能 | 对应 Nginx | 说明 |
|------|-----------|------|
| `if` 条件块 | Nginx `if` | 配置层条件逻辑（谨慎实现，Nginx if 语义复杂） |
| `geo` 模块 | `geo` | 基于 IP 段的变量/路由分发 |
| 大文件 Range 分片缓存 | `proxy_cache` + `slice` | 大文件按 Range 分片缓存，减少上游回源 |
| OpenTelemetry 追踪 | — | 分布式追踪（Jaeger / Zipkin / OTLP） |

### 低优先级

| 功能 | 说明 |
|------|------|
| `map` 变量 | 配置层变量映射 |
| Prometheus 指标主动推送 | 当前仅支持拉取（`/metrics`） |
| QUIC 0-RTT 支持 | 首请求免握手，注意重放攻击风险 |
| 配置 Web UI | 可选的图形化配置界面 |

---

## 横向对比

| 项目 | Sweety | Nginx | Caddy | Apache |
|------|--------|-------|-------|--------|
| HTTP/3 内置 | ✅ | ❌ 需重编译 | ✅ | ❌ 实验模块 |
| ACME 自动证书 | ✅ | ❌ 需 certbot | ✅ | ❌ 需插件 |
| Brotli 压缩 | ✅ 内置 | ❌ 第三方模块 | ✅ | ✅ mod_brotli |
| 断路器 | ✅ 三状态机 | ⚠️ max_fails 仅计数 | ❌ | ❌ |
| WebSocket 代理 | ✅ | ✅ | ✅ | ✅ mod_proxy_wstunnel |
| gRPC 代理 | ✅ | ✅（商业版全功能） | ✅ | ⚠️ 有限支持 |
| 反向代理连接池 | ✅ | ✅ | ✅ | ✅ |
| 静态文件内存缓存 | ✅ | ✅ OS page cache | ❌ | ✅ mod_cache |
| FastCGI 响应缓存 | ✅ | ✅ | ❌ | ✅ mod_cache_disk |
| H2 多核扩展 | ✅ SO_REUSEPORT | ✅ | ✅ | ✅ |
| 配置易用性 | ✅ 预设 + 语法糖 | ❌ 纯手写 | ✅ Caddyfile | ⚠️ 冗长 |
| 配置热重载 | ✅ 不断连 | ✅ | ✅ | ✅ graceful |
| `if` / `map` 条件 | ❌ | ✅ | ⚠️ 有限 | ✅ mod_rewrite |
| TCP/UDP 四层代理 | ❌ | ✅ stream | ❌ | ❌ |
| `.htaccess` 目录级配置 | ❌ | ❌ | ❌ | ✅ |
| 内存安全 | ✅ Rust | ❌ C | ✅ Go | ❌ C |
| 单文件无依赖 | ✅ | ❌ | ✅ | ❌ |
| **生产检验** | ⚠️ **未验证** | ✅ 广泛 | ✅ 广泛 | ✅ 广泛 |

---

*最后更新：2026-04-03*
