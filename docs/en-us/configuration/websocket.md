# WebSocket

## Configuration

```toml
[[sites]]
name        = "ws-site"
server_name = ["ws.example.com"]
listen      = [80]
listen_tls  = [443]
websocket   = true       # Site-level toggle, default true
acme_email  = "your@email.com"

[[sites.upstreams]]
name  = "ws-backend"
nodes = [{ addr = "127.0.0.1:8080" }]

[[sites.locations]]
path     = "/ws"
handler  = "websocket"
upstream = "ws-backend"
```

## Mixed HTTP + WebSocket

A single site can serve both HTTP and WebSocket:

```toml
# WebSocket upgrade path
[[sites.locations]]
path     = "/ws"
handler  = "websocket"
upstream = "ws-backend"

# Regular HTTP reverse proxy
[[sites.locations]]
path     = "/api/"
handler  = "reverse_proxy"
upstream = "api-backend"

# Static files
[[sites.locations]]
path    = "/"
handler = "static"
```

## Connection Limits

```toml
[[sites.locations]]
path            = "/ws"
handler         = "websocket"
upstream        = "ws-backend"
max_connections = 1000   # Max concurrent WebSocket connections
```

## Forwarding Headers

Request header forwarding during WebSocket handshake (HTTP Upgrade):

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

## WSS (WebSocket over TLS)

When clients connect to `wss://ws.example.com/ws`, Sweety handles TLS termination automatically — upstream remains plain TCP:

```toml
listen_tls = [443]   # External WSS

[[sites.upstreams.nodes]]
addr = "127.0.0.1:8080"   # Upstream is plaintext WS
```

If upstream also requires TLS:

```toml
[[sites.upstreams.nodes]]
addr = "ws-backend.internal:443"
tls  = true
tls_sni = "ws-backend.internal"
```

## Notes

- WebSocket connections are long-lived and not subject to `keepalive_timeout`
- `websocket = false` can disable WebSocket support at the site level
- HTTP/2 over WebSocket (RFC 8441) is also supported
