# gRPC 代理

Sweety 支持透明转发 gRPC 和 gRPC-Web 请求。gRPC 基于 HTTP/2，因此上游需要支持 HTTP/2。

## 基本配置

```toml
[[sites]]
name        = "grpc-gateway"
server_name = ["grpc.example.com"]
listen_tls  = [443]
acme_email  = "your@email.com"

# 上游 gRPC 服务（HTTP/2，通常不加密）
[[sites.upstreams]]
name = "grpc-backend"

[[sites.upstreams.nodes]]
addr  = "127.0.0.1:50051"
http2 = true      # 使用 HTTP/2 连接上游

# gRPC 路由
[[sites.locations]]
path     = "/"
handler  = "grpc"
upstream = "grpc-backend"
```

## gRPC over TLS 上游

```toml
[[sites.upstreams.nodes]]
addr     = "grpc-service.internal:50051"
http2    = true
tls      = true
tls_sni  = "grpc-service.internal"
```

## 按服务路由

gRPC 请求路径格式为 `/<package>.<Service>/<Method>`，可以按服务或方法精细路由：

```toml
# 路由 UserService 到 user-backend
[[sites.locations]]
path     = "/com.example.UserService/"
handler  = "grpc"
upstream = "user-backend"

# 路由 OrderService 到 order-backend
[[sites.locations]]
path     = "/com.example.OrderService/"
handler  = "grpc"
upstream = "order-backend"

# 默认路由
[[sites.locations]]
path     = "/"
handler  = "grpc"
upstream = "default-backend"
```

## gRPC-Web

浏览器端的 gRPC-Web 客户端（基于 HTTP/1.1 或 HTTP/2 的封装协议）无需特殊配置，Sweety 自动识别 `Content-Type: application/grpc-web` 并透明转发。

## 超时配置

```toml
[[sites.upstreams]]
name            = "grpc-backend"
connect_timeout = 5
read_timeout    = 300   # gRPC 流式调用需要较长超时
write_timeout   = 300
```

## 转发头

```toml
[[sites.locations]]
path     = "/"
handler  = "grpc"
upstream = "grpc-backend"

[[sites.locations.proxy_set_headers]]
name  = "X-Real-IP"
value = "$remote_addr"

[[sites.locations.proxy_set_headers]]
name  = "X-Forwarded-For"
value = "$remote_addr"
```

## 注意事项

- gRPC 要求 HTTP/2，客户端到 Sweety 必须通过 HTTPS（TLS）
- 上游若为 h2c（明文 HTTP/2）：节点设 `http2 = true`，`tls = false`
- 上游若为 gRPC over TLS：节点设 `http2 = true`，`tls = true`
- `read_timeout` / `write_timeout` 建议设为较大值（如 300 秒），避免流式 RPC 超时中断
