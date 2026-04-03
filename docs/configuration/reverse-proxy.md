# 反向代理

## 基本配置

```toml
[[sites.upstreams]]
name  = "backend"
nodes = [{ addr = "127.0.0.1:3000" }]

[[sites.locations]]
path     = "/"
handler  = "reverse_proxy"
upstream = "backend"
```

## 完整上游（upstream）配置

```toml
[[sites.upstreams]]
name     = "backend"
strategy = "round_robin"   # round_robin / weighted / least_conn / ip_hash

# ─── 节点列表 ───────────────────────────────────────────────────
[[sites.upstreams.nodes]]
addr           = "10.0.0.1:8080"
weight         = 10            # 加权轮询时有效（默认 1）
tls            = false         # 是否 TLS 连接上游
tls_sni        = "backend.internal"  # TLS SNI（不设则用 addr 的 host）
tls_insecure   = false         # 跳过上游证书验证
upstream_host  = "backend.internal"  # 发送给上游的 Host 头
http2          = false         # 使用 HTTP/2 连接上游

[[sites.upstreams.nodes]]
addr   = "10.0.0.2:8080"
weight = 5

# ─── 连接池 ─────────────────────────────────────────────────────
keepalive          = 32    # 空闲连接池大小（等价 Nginx keepalive）
keepalive_requests = 1000  # 单连接最大复用请求数
keepalive_time     = 600   # 连接最大复用时间（秒，0 = 不限制）

# ─── 超时 ───────────────────────────────────────────────────────
connect_timeout = 10   # 连接超时（秒，默认 10）
read_timeout    = 60   # 读取超时（秒，默认 60）
write_timeout   = 60   # 写入超时（秒，默认 60）

# ─── 重试 ───────────────────────────────────────────────────────
retry         = 2    # 失败重试次数
retry_timeout = 0    # 重试前等待（秒，0 = 立即）

# ─── 断路器 ─────────────────────────────────────────────────────
[sites.upstreams.circuit_breaker]
max_failures = 5    # 时间窗口内最大失败次数
window_secs  = 60   # 时间窗口（秒）
fail_timeout = 30   # 开路后恢复探测间隔（秒）

# ─── 健康检查 ───────────────────────────────────────────────────
[sites.upstreams.health_check]
enabled  = true
interval = 10         # 检查间隔（秒）
timeout  = 3          # 超时（秒）
path     = "/health"  # 检查路径
```

## 负载均衡策略

| 策略 | 值 | 说明 |
|------|----|----|
| 轮询（默认） | `round_robin` | 依次分发，等价 Nginx `upstream {}` 默认 |
| 加权轮询 | `weighted` | 按 `weight` 字段比例分发 |
| 最少连接 | `least_conn` | 分发到活跃连接最少的节点 |
| IP 哈希 | `ip_hash` | 同 IP 路由到同一节点（会话粘滞） |

## HTTPS 上游

```toml
[[sites.upstreams.nodes]]
addr   = "secure-backend.internal:443"
tls    = true
tls_sni = "secure-backend.internal"
# tls_insecure = true   # 自签名证书时开启
```

## HTTP/2 上游（gRPC 等）

```toml
[[sites.upstreams.nodes]]
addr  = "grpc-backend:50051"
http2 = true
tls   = true
```

## 常用 Location 配置

### 转发所有请求

```toml
[[sites.locations]]
path     = "/"
handler  = "reverse_proxy"
upstream = "backend"

[[sites.locations.proxy_set_headers]]
name  = "X-Real-IP"
value = "$remote_addr"

[[sites.locations.proxy_set_headers]]
name  = "X-Forwarded-For"
value = "$remote_addr"

[[sites.locations.proxy_set_headers]]
name  = "X-Forwarded-Proto"
value = "$scheme"

[[sites.locations.proxy_set_headers]]
name  = "Host"
value = "$host"
```

### 路径前缀转发

```toml
# /api/* → backend:3000
[[sites.locations]]
path     = "/api/"
handler  = "reverse_proxy"
upstream = "api-backend"

# /admin/* → admin-backend:8080
[[sites.locations]]
path     = "/admin/"
handler  = "reverse_proxy"
upstream = "admin-backend"

# /* → 静态文件
[[sites.locations]]
path    = "/"
handler = "static"
```

### SSE / 流式响应

```toml
[[sites.locations]]
path            = "/events"
handler         = "reverse_proxy"
upstream        = "backend"
proxy_buffering = false   # 关闭缓冲，确保实时推送
```

## 反代缓存（proxy_cache）

```toml
[sites.proxy_cache]
max_entries        = 1000
ttl                = 60
cacheable_statuses = [200]
cacheable_methods  = ["GET", "HEAD"]
bypass_headers     = ["Authorization", "Cookie"]
ignore_headers     = []
```
