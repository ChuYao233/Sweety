# Reverse Proxy

## Basic Configuration

```toml
[[sites.upstreams]]
name  = "backend"
nodes = [{ addr = "127.0.0.1:3000" }]

[[sites.locations]]
path     = "/"
handler  = "reverse_proxy"
upstream = "backend"
```

## Full Upstream Configuration

```toml
[[sites.upstreams]]
name     = "backend"
strategy = "round_robin"   # round_robin / weighted / least_conn / ip_hash

# ─── Node List ────────────────────────────────────────────────────
[[sites.upstreams.nodes]]
addr           = "10.0.0.1:8080"
weight         = 10            # Effective for weighted round-robin (default 1)
tls            = false         # TLS connection to upstream
tls_sni        = "backend.internal"  # TLS SNI (defaults to addr host)
tls_insecure   = false         # Skip upstream certificate verification
upstream_host  = "backend.internal"  # Host header sent to upstream
http2          = false         # Use HTTP/2 to connect upstream

[[sites.upstreams.nodes]]
addr   = "10.0.0.2:8080"
weight = 5

# ─── Connection Pool ─────────────────────────────────────────────
keepalive          = 32    # Idle connection pool size (equivalent to Nginx keepalive)
keepalive_requests = 1000  # Max requests per connection
keepalive_time     = 600   # Max connection reuse time (seconds, 0 = unlimited)

# ─── Timeouts ────────────────────────────────────────────────────
connect_timeout = 10   # Connect timeout (seconds, default 10)
read_timeout    = 60   # Read timeout (seconds, default 60)
write_timeout   = 60   # Write timeout (seconds, default 60)

# ─── Retries ─────────────────────────────────────────────────────
retry         = 2    # Retry count on failure
retry_timeout = 0    # Wait before retry (seconds, 0 = immediate)

# ─── Circuit Breaker ─────────────────────────────────────────────
[sites.upstreams.circuit_breaker]
max_failures = 5    # Max failures within time window
window_secs  = 60   # Time window (seconds)
fail_timeout = 30   # Recovery probe interval after opening (seconds)

# ─── Health Check ────────────────────────────────────────────────
[sites.upstreams.health_check]
enabled  = true
interval = 10         # Check interval (seconds)
timeout  = 3          # Timeout (seconds)
path     = "/health"  # Check path
```

## Load Balancing Strategies

| Strategy | Value | Description |
|----------|-------|-------------|
| Round Robin (default) | `round_robin` | Distribute sequentially, equivalent to Nginx `upstream {}` default |
| Weighted Round Robin | `weighted` | Distribute by `weight` field ratio |
| Least Connections | `least_conn` | Route to node with fewest active connections |
| IP Hash | `ip_hash` | Same IP routes to same node (session stickiness) |

## HTTPS Upstream

```toml
[[sites.upstreams.nodes]]
addr   = "secure-backend.internal:443"
tls    = true
tls_sni = "secure-backend.internal"
# tls_insecure = true   # Enable for self-signed certificates
```

## HTTP/2 Upstream (gRPC, etc.)

```toml
[[sites.upstreams.nodes]]
addr  = "grpc-backend:50051"
http2 = true
tls   = true
```

## Common Location Configurations

### Forward All Requests

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

### Path Prefix Routing

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

# /* → static files
[[sites.locations]]
path    = "/"
handler = "static"
```

### SSE / Streaming Responses

```toml
[[sites.locations]]
path            = "/events"
handler         = "reverse_proxy"
upstream        = "backend"
proxy_buffering = false   # Disable buffering for real-time push
```

## Proxy Cache (proxy_cache)

```toml
[sites.proxy_cache]
max_entries        = 1000
ttl                = 60
cacheable_statuses = [200]
cacheable_methods  = ["GET", "HEAD"]
bypass_headers     = ["Authorization", "Cookie"]
ignore_headers     = []
```
