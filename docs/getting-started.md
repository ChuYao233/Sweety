# Getting Started

## Build

```bash
cargo build --release
# Binary: target/release/sweety
```

## CLI

```
Usage: sweety [OPTIONS] [COMMAND]

Commands:
  run       Start Sweety in foreground (recommended for production)
  validate  Validate config file and TLS certificates (equivalent to nginx -t)
  reload    Hot-reload config without dropping connections
  api-doc   Output Admin REST API documentation as JSON
  version   Print version info

Options:
  -c, --config <FILE>  Config file path [default: config/sweety.toml]
```

```bash
# Start (default: config/sweety.toml)
sweety run

# Specify config file
sweety run --config /etc/sweety/sweety.toml

# Validate config + TLS certs before deploy
sweety validate

# Hot-reload (no connection drops)
sweety reload
```

## Minimal Static Site

```toml
[[sites]]
name        = "my-site"
server_name = ["example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/example"
force_https = true

[sites.tls]
acme       = true
acme_email = "admin@example.com"

[[sites.locations]]
path    = "/"
handler = "static"
```

## PHP + WordPress

```toml
[[sites]]
name        = "wordpress"
server_name = ["blog.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/wordpress"
index       = ["index.php", "index.html"]
force_https = true

[sites.tls]
acme       = true
acme_email = "admin@example.com"

[sites.fastcgi]
socket = "/var/run/php/php8.2-fpm.sock"

[[sites.rewrites]]
pattern   = "^/(.+)$"
target    = "/index.php?$1"
flag      = "last"
condition = "!-f"

[[sites.locations]]
path    = "~ \\.php$"
handler = "fastcgi"

[[sites.locations]]
path      = "/"
handler   = "fastcgi"
try_files = ["$uri", "$uri/", "/index.php"]
```

## Reverse Proxy with Load Balancing

```toml
[[sites]]
name        = "api"
server_name = ["api.example.com"]
listen      = [80]
listen_tls  = [443]
force_https = true

[sites.tls]
acme       = true
acme_email = "admin@example.com"

[[sites.upstreams]]
name            = "backend"
strategy        = "least_conn"
connect_timeout = 5
read_timeout    = 30
retry           = 2

[sites.upstreams.circuit_breaker]
max_failures = 5
window_secs  = 60
fail_timeout = 30

[[sites.upstreams.nodes]]
addr = "127.0.0.1:8001"

[[sites.upstreams.nodes]]
addr   = "127.0.0.1:8002"
weight = 2

[[sites.locations]]
path     = "/"
handler  = "reverse_proxy"
upstream = "backend"
```

## WebSocket Proxy

```toml
[[sites]]
name        = "ws-app"
server_name = ["ws.example.com"]
listen      = [80]
listen_tls  = [443]
websocket   = true

[sites.tls]
acme = true

[[sites.upstreams]]
name = "ws-backend"
[[sites.upstreams.nodes]]
addr = "127.0.0.1:8080"

[[sites.locations]]
path     = "/ws/"
handler  = "reverse_proxy"
upstream = "ws-backend"
```

Supports H1 Upgrade (RFC 6455) and H2 extended CONNECT (RFC 8441) transparently.

Full configuration reference: [`config/sweety.example.toml`](../config/sweety.example.toml)
