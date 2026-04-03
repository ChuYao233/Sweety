# 示例：gRPC 代理

## 基本 gRPC 代理

```toml
[[sites]]
name        = "grpc-gateway"
server_name = ["grpc.example.com"]
listen_tls  = [443]
acme_email  = "your@email.com"

[[sites.upstreams]]
name = "grpc-backend"

[[sites.upstreams.nodes]]
addr  = "127.0.0.1:50051"
http2 = true     # gRPC 必须使用 HTTP/2

[[sites.locations]]
path     = "/"
handler  = "grpc"
upstream = "grpc-backend"
```

---

## 按 gRPC 服务路由

gRPC 请求路径格式：`/<包名>.<服务名>/<方法名>`

```toml
[[sites]]
name        = "grpc-multi"
server_name = ["grpc.example.com"]
listen_tls  = [443]
acme_email  = "your@email.com"

[[sites.upstreams]]
name  = "user-svc"
nodes = [{ addr = "127.0.0.1:50051", http2 = true }]

[[sites.upstreams]]
name  = "order-svc"
nodes = [{ addr = "127.0.0.1:50052", http2 = true }]

[[sites.upstreams]]
name  = "product-svc"
nodes = [{ addr = "127.0.0.1:50053", http2 = true }]

# UserService → user-svc
[[sites.locations]]
path     = "/com.example.user.UserService/"
handler  = "grpc"
upstream = "user-svc"

# OrderService → order-svc
[[sites.locations]]
path     = "/com.example.order.OrderService/"
handler  = "grpc"
upstream = "order-svc"

# 其余 → product-svc
[[sites.locations]]
path     = "/"
handler  = "grpc"
upstream = "product-svc"
```

---

## gRPC + REST API 混合

同一域名同时提供 gRPC 和 REST：

```toml
[[sites]]
name        = "api-gateway"
server_name = ["api.example.com"]
listen_tls  = [443]
acme_email  = "your@email.com"

[[sites.upstreams]]
name  = "grpc-svc"
nodes = [{ addr = "127.0.0.1:50051", http2 = true }]

[[sites.upstreams]]
name  = "rest-svc"
nodes = [{ addr = "127.0.0.1:3000" }]

# gRPC 请求（Content-Type: application/grpc）
[[sites.locations]]
path     = "~ ^/com\\.example\\."
handler  = "grpc"
upstream = "grpc-svc"

# REST 请求
[[sites.locations]]
path     = "/"
handler  = "reverse_proxy"
upstream = "rest-svc"
```

---

## gRPC 负载均衡

```toml
[[sites.upstreams]]
name     = "grpc-cluster"
strategy = "round_robin"

[[sites.upstreams.nodes]]
addr  = "10.0.0.1:50051"
http2 = true

[[sites.upstreams.nodes]]
addr  = "10.0.0.2:50051"
http2 = true

[[sites.upstreams.nodes]]
addr  = "10.0.0.3:50051"
http2 = true

[sites.upstreams.health_check]
enabled  = true
interval = 10
timeout  = 3
path     = "/grpc.health.v1.Health/Check"
```

---

## gRPC over TLS 上游

```toml
[[sites.upstreams.nodes]]
addr    = "grpc-svc.internal:443"
http2   = true
tls     = true
tls_sni = "grpc-svc.internal"
```

---

## 流式 RPC 超时配置

gRPC 流式调用（Server Streaming / Bidirectional Streaming）需要较长超时：

```toml
[[sites.upstreams]]
name            = "grpc-stream"
connect_timeout = 5
read_timeout    = 3600    # 流式 RPC 可能持续很久
write_timeout   = 3600
nodes           = [{ addr = "127.0.0.1:50051", http2 = true }]
```

---

## 客户端示例（grpcurl）

```bash
# 安装 grpcurl
go install github.com/fullstorydev/grpcurl/cmd/grpcurl@latest

# 列出服务
grpcurl grpc.example.com:443 list

# 调用方法
grpcurl -d '{"name": "world"}' grpc.example.com:443 helloworld.Greeter/SayHello
```
