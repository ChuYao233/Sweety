# gRPC Proxy

Sweety supports transparent forwarding of gRPC and gRPC-Web requests. gRPC is based on HTTP/2, so the upstream must support HTTP/2.

## Basic Configuration

```toml
[[sites]]
name        = "grpc-gateway"
server_name = ["grpc.example.com"]
listen_tls  = [443]
acme_email  = "your@email.com"

# Upstream gRPC service (HTTP/2, typically unencrypted)
[[sites.upstreams]]
name = "grpc-backend"

[[sites.upstreams.nodes]]
addr  = "127.0.0.1:50051"
http2 = true      # Use HTTP/2 to connect upstream

# gRPC route
[[sites.locations]]
path     = "/"
handler  = "grpc"
upstream = "grpc-backend"
```

## gRPC over TLS Upstream

```toml
[[sites.upstreams.nodes]]
addr     = "grpc-service.internal:50051"
http2    = true
tls      = true
tls_sni  = "grpc-service.internal"
```

## Per-Service Routing

gRPC request paths follow the format `/<package>.<Service>/<Method>`, allowing fine-grained routing by service or method:

```toml
# Route UserService to user-backend
[[sites.locations]]
path     = "/com.example.UserService/"
handler  = "grpc"
upstream = "user-backend"

# Route OrderService to order-backend
[[sites.locations]]
path     = "/com.example.OrderService/"
handler  = "grpc"
upstream = "order-backend"

# Default route
[[sites.locations]]
path     = "/"
handler  = "grpc"
upstream = "default-backend"
```

## gRPC-Web

Browser-side gRPC-Web clients (wrapped protocol over HTTP/1.1 or HTTP/2) require no special configuration. Sweety automatically detects `Content-Type: application/grpc-web` and forwards transparently.

## Timeout Configuration

```toml
[[sites.upstreams]]
name            = "grpc-backend"
connect_timeout = 5
read_timeout    = 300   # Streaming gRPC calls need longer timeouts
write_timeout   = 300
```

## Forwarding Headers

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

## Notes

- gRPC requires HTTP/2 — client-to-Sweety must use HTTPS (TLS)
- For h2c (plaintext HTTP/2) upstream: set `http2 = true`, `tls = false`
- For gRPC over TLS upstream: set `http2 = true`, `tls = true`
- Set `read_timeout` / `write_timeout` to large values (e.g. 300 seconds) to avoid streaming RPC timeout interruptions
