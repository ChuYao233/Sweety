# WebSocket

## 配置

```toml
[[sites]]
name        = "ws-site"
server_name = ["ws.example.com"]
listen      = [80]
listen_tls  = [443]
websocket   = true       # 站点级开关，默认 true
acme_email  = "your@email.com"

[[sites.upstreams]]
name  = "ws-backend"
nodes = [{ addr = "127.0.0.1:8080" }]

[[sites.locations]]
path     = "/ws"
handler  = "websocket"
upstream = "ws-backend"
```

## 混合 HTTP + WebSocket

同一站点可同时提供 HTTP 和 WebSocket：

```toml
# WebSocket 升级路径
[[sites.locations]]
path     = "/ws"
handler  = "websocket"
upstream = "ws-backend"

# 普通 HTTP 反向代理
[[sites.locations]]
path     = "/api/"
handler  = "reverse_proxy"
upstream = "api-backend"

# 静态文件
[[sites.locations]]
path    = "/"
handler = "static"
```

## 连接数限制

```toml
[[sites.locations]]
path            = "/ws"
handler         = "websocket"
upstream        = "ws-backend"
max_connections = 1000   # 最大并发 WebSocket 连接数
```

## 转发头

WebSocket 握手阶段（HTTP Upgrade）的请求头转发：

```toml
[[sites.locations]]
path     = "/ws"
handler  = "websocket"
upstream = "ws-backend"

[[sites.locations.proxy_set_headers]]
name  = "X-Real-IP"
value = "$remote_addr"

[[sites.locations.proxy_set_headers]]
name  = "X-Forwarded-Proto"
value = "$scheme"
```

## WSS（WebSocket over TLS）

客户端连接 `wss://ws.example.com/ws` 时，Sweety 自动处理 TLS 解包，上游仍为普通 TCP：

```toml
listen_tls = [443]   # 对外 WSS

[[sites.upstreams.nodes]]
addr = "127.0.0.1:8080"   # 上游为明文 WS
```

若上游也需要 TLS：

```toml
[[sites.upstreams.nodes]]
addr = "ws-backend.internal:443"
tls  = true
tls_sni = "ws-backend.internal"
```

## 注意事项

- WebSocket 连接是长连接，不受 `keepalive_timeout` 约束
- `websocket = false` 可在站点级禁用 WebSocket 支持
- HTTP/2 over WebSocket（RFC 8441）同样支持
