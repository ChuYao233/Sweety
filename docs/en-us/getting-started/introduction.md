# Introduction & Features

## What is Sweety

Sweety is a high-performance, multi-site web server written in Rust, aiming to combine **Nginx-level deep configurability** with **Caddy-style out-of-the-box experience**.

## Core Features

### Protocol Support
- **HTTP/1.1** — Keep-Alive, Pipeline
- **HTTP/2** — Multiplexing, Server Push (h2 over TLS)
- **HTTP/3 / QUIC** — Based on quinn, shares the same port (443) with HTTP/2

### TLS
- Manual certificates (cert/key files)
- **ACME automatic certificates**: Let's Encrypt / ZeroSSL / Buypass / LiteSSL, supports HTTP-01 and DNS-01 validation
- Multi-certificate (SNI routing, different certificates for different domains on the same port)
- HSTS, TLS version/cipher suite control

### Site Features
| Feature | Description |
|---------|-------------|
| Static Files | In-memory cache, Range, gzip/brotli compression |
| FastCGI/PHP | Connection pool, Unix socket/TCP, response cache |
| Reverse Proxy | HTTP/1.1 + HTTP/2 upstream, connection pool, circuit breaker, load balancing |
| gRPC Proxy | Transparent gRPC/gRPC-Web forwarding |
| WebSocket | Forward proxy WS/WSS |
| auth_request | Subrequest authentication (equivalent to Nginx auth_request) |
| Rate Limiting | IP or Header-based request rate limiting |
| Rewrite | Regex URL rewriting (last / break / redirect / permanent) |
| Error Pages | Custom `error_pages` |
| HTTPS Redirect | `force_https = true` |

### Out of the Box (Caddy-style Sugar Syntax)
- `preset = "wordpress"` — One line to enable optimal WordPress location rules
- `php_fastcgi = "/tmp/php.sock"` — One line to replace a full `[sites.fastcgi]` block
- `acme_email = "you@example.com"` — One line to enable ACME automatic HTTPS

### Operations
- **Hot Reload**: `sweety reload` reloads config without dropping connections
- **Daemon Mode**: `sweety start/stop/restart`
- **Config Validation**: `sweety validate` (equivalent to `nginx -t`)
- **Prometheus Metrics**: `/metrics` endpoint (planned for v0.5)
- **Admin REST API**: health / stats / plugins available (`/api/v1/*`); site management and node control planned for v0.5

## Comparison with Alternatives

| | Sweety | Nginx | Caddy |
|---|---|---|---|
| Language | Rust | C | Go |
| HTTP/3 | ✅ Native | Requires patch | ✅ Native |
| ACME Auto-Cert | ✅ | ❌ (needs plugin) | ✅ |
| Config Format | TOML/JSON/YAML | Custom syntax | Caddyfile/JSON |
| Hot Reload | ✅ | ✅ | ✅ |
| WebSocket | ✅ | ✅ | ✅ |
| gRPC Proxy | ✅ | ✅ (full in Plus) | ✅ |
| Memory Safety | ✅ | ❌ | ✅ |
| Static File Memory Cache | ✅ | ✅ | ❌ |
| FastCGI Response Cache | ✅ | ✅ | ❌ |
