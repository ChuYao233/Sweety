# 全局配置 [global]

全局配置影响所有站点，所有字段均有默认值，`[global]` 块可完全省略。

## 完整配置项

```toml
[global]
# ─── 线程与连接 ─────────────────────────────────────────────────
worker_threads     = 0       # Worker 线程数，0 = 自动（CPU 核心数）
worker_connections = 51200   # 每 worker 最大并发连接数
max_connections    = 0       # 全局最大连接数，0 = 不限制
keepalive_timeout  = 60      # Keep-Alive 超时（秒）

# ─── 请求限制 ───────────────────────────────────────────────────
client_max_body_size       = 50    # 最大请求体（MB）
client_header_buffer_size  = 32    # 请求头缓冲区（KB）
client_body_buffer_size    = 512   # 请求体缓冲区（KB）

# ─── FastCGI 全局默认超时 ───────────────────────────────────────
fastcgi_connect_timeout = 5    # 连接超时（秒）
fastcgi_read_timeout    = 60   # 读取超时（秒）

# ─── 压缩 ───────────────────────────────────────────────────────
gzip            = false  # 全局启用 gzip
gzip_min_length = 1      # 最小压缩大小（KB）
gzip_comp_level = 5      # 压缩等级 1-9

# ─── HTTP/2 ─────────────────────────────────────────────────────
h2_max_concurrent_streams       = 128   # 单连接最大并发流
h2_max_pending_per_conn         = 0     # 最大排队请求数（0 = 不限制）
h2_max_concurrent_reset_streams = 200   # RST 洪水防护
h2_max_frame_size               = 65535 # 最大帧大小（字节）
h2_max_requests_per_conn        = 1000  # 单连接最大请求数（0 = 不限制）

# ─── HTTP/3 ─────────────────────────────────────────────────────
h3_max_handlers = 0   # 全局最大并发 H3 handler 数（0 = 自动，按可用内存 80% / 2MB 计算）

# ─── 日志 ───────────────────────────────────────────────────────
log_level = "info"      # error / warn / info / debug / trace
error_log = "/var/log/sweety/error.log"  # 错误日志路径（可选）

# ─── 管理 API ───────────────────────────────────────────────────
admin_listen = "127.0.0.1:9099"   # Admin API 监听地址（空 = 禁用）
admin_token  = "your-secret-token" # Bearer Token 鉴权

# ─── Prometheus 指标 ────────────────────────────────────────────
prometheus_enabled = true
prometheus_path    = "/metrics"    # 挂载在 admin_listen 上
```

## 字段说明

### 线程与连接

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `worker_threads` | `0` | `0` 自动取 CPU 核心数，等价 `nginx worker_processes auto` |
| `worker_connections` | `51200` | 等价 `nginx worker_connections` |
| `max_connections` | `0` | 总并发连接上限，`0` 不限制 |
| `keepalive_timeout` | `60` | TCP Keep-Alive 超时，`0` 禁用 |

### 请求限制

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `client_max_body_size` | `50` MB | 超出返回 `413`，等价 `nginx client_max_body_size` |
| `client_header_buffer_size` | `32` KB | 请求头缓冲区 |
| `client_body_buffer_size` | `512` KB | 请求体缓冲区 |

### HTTP/2

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `h2_max_concurrent_streams` | `128` | 单连接最大并发请求数，等价 `nginx http2_max_concurrent_streams` |
| `h2_max_pending_per_conn` | `0` | 单连接最大在途 handler 数，`0` 不限制。限制过多并发 handler 可降低内存峰值 |
| `h2_max_concurrent_reset_streams` | `200` | 防 RST Flood 攻击（CVE-2023-44487） |
| `h2_max_frame_size` | `65535` | HTTP/2 帧大小，影响大文件传输效率 |
| `h2_max_requests_per_conn` | `1000` | 连接复用上限，超出后关闭连接，`0` 不限制 |

### HTTP/3

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `h3_max_handlers` | `0` | 全局最大并发 H3 handler 数。`0` = 自动计算（系统可用内存 80% / 2MB）。每个 QUIC 连接最多缓冲 `send_window` 字节，此限制防止 OOM。超出时新连接排队而非拒绝 |

### Admin API

配置 `admin_listen` 后可使用 REST API（`/api/v1/*`）：

**已实现：**
- `GET /api/v1/health` — 健康检查
- `GET /api/v1/version` — 版本信息
- `GET /api/v1/stats` — 全局请求统计快照
- `GET /api/v1/plugins` — 已注册插件列表
- `GET /api/v1/doc` — API 文档（JSON）
- Bearer Token 鉴权

**v0.5 计划：**
- 站点管理、上游节点控制、API 热重载
- WebSocket 实时统计推送
- Prometheus `/metrics` 端点

安全建议：只监听 `127.0.0.1`，**不要暴露到公网**。
