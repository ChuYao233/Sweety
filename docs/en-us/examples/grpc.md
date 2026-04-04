# Example: gRPC Proxy

## Basic gRPC Proxy

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
http2 = true     # gRPC requires HTTP/2

[[sites.locations]]
path     = "/"
handler  = "grpc"
upstream = "grpc-backend"
```

---

## Per-Service Routing

gRPC request path format: `/<package>.<Service>/<Method>`

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

# Default → product-svc
[[sites.locations]]
path     = "/"
handler  = "grpc"
upstream = "product-svc"
```

---

## gRPC + REST API Mixed

Same domain serving both gRPC and REST:

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

# gRPC requests (Content-Type: application/grpc)
[[sites.locations]]
path     = "~ ^/com\\.example\\."
handler  = "grpc"
upstream = "grpc-svc"

# REST requests
[[sites.locations]]
path     = "/"
handler  = "reverse_proxy"
upstream = "rest-svc"
```

---

## gRPC Load Balancing

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

## gRPC over TLS Upstream

```toml
[[sites.upstreams.nodes]]
addr    = "grpc-svc.internal:443"
http2   = true
tls     = true
tls_sni = "grpc-svc.internal"
```

---

## Streaming RPC Timeout Configuration

gRPC streaming calls (Server Streaming / Bidirectional Streaming) need longer timeouts:

```toml
[[sites.upstreams]]
name            = "grpc-stream"
connect_timeout = 5
read_timeout    = 3600    # Streaming RPCs may last a long time
write_timeout   = 3600
nodes           = [{ addr = "127.0.0.1:50051", http2 = true }]
```

---

## Client Example (grpcurl)

```bash
# Install grpcurl
go install github.com/fullstorydev/grpcurl/cmd/grpcurl@latest

# List services
grpcurl grpc.example.com:443 list

# Call method
grpcurl -d '{"name": "world"}' grpc.example.com:443 helloworld.Greeter/SayHello
```
