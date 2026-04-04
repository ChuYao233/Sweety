# Routing Rules [[sites.locations]]

`locations` define URL path matching rules and their corresponding handlers, equivalent to Nginx `location` blocks.

## Match Syntax

| Prefix | Type | Example |
|--------|------|---------|
| `= /path` | Exact match (highest priority) | `= /favicon.ico` |
| `^~ /prefix` | Prefix match (skip regex) | `^~ /static/` |
| `~ regex` | Regex match (case-sensitive) | `~ \.php$` |
| `~* regex` | Regex match (case-insensitive) | `~* \.(jpg\|png)$` |
| `/prefix` | Plain prefix match | `/api/` |

Priority: Exact `=` > Prefix `^~` > Regex `~`/`~*` > Plain prefix.

## Handler Types

| Value | Description |
|-------|-------------|
| `static` | Static file serving (default) |
| `fastcgi` | PHP / FastCGI forwarding |
| `reverse_proxy` | HTTP reverse proxy |
| `grpc` | gRPC proxy |
| `websocket` | WebSocket proxy |
| `plugin:<name>` | Custom plugin |

## Full Configuration

```toml
[[sites.locations]]
path    = "/api/"
handler = "reverse_proxy"
upstream = "backend"         # References [[sites.upstreams]] name

# ─── Root Override ────────────────────────────────────────────────
root = "/var/www/other"       # Override site-level root

# ─── Direct Return ───────────────────────────────────────────────
return_code = 200             # Return specified status code (no body)
return_url  = "https://new.example.com$request_uri"  # Redirect
return_body = "OK"            # Return text content
return_content_type = "application/json"

# ─── File Lookup ──────────────────────────────────────────────────
try_files = ["$uri", "$uri/", "/index.php?$args"]  # Equivalent to Nginx try_files

# ─── Response Header Control ─────────────────────────────────────
cache_control = "public, max-age=86400"

[[sites.locations.add_headers]]
name  = "X-Frame-Options"
value = "DENY"

[[sites.locations.proxy_set_headers]]
name  = "X-Real-IP"
value = "$remote_addr"

[[sites.locations.proxy_set_headers]]
name  = "X-Forwarded-Proto"
value = "$scheme"

# ─── Cache Rules (by extension) ──────────────────────────────────
[[sites.locations.cache_rules]]
pattern       = "\\.(css|js|woff2?)$"
cache_control = "public, max-age=2592000, immutable"

[[sites.locations.cache_rules]]
pattern       = "\\.(png|jpg|gif|webp|svg|ico)$"
cache_control = "public, max-age=2592000"

# ─── Connection Limits ───────────────────────────────────────────
limit_conn      = 100         # Concurrent connection limit (0 = unlimited)
max_connections = 50          # WebSocket-specific max connections

# ─── Subrequest Auth (auth_request) ──────────────────────────────
auth_request        = "/auth-check"   # Auth subrequest path
auth_failure_status = 401             # Failure response status code

[[sites.locations.auth_request_headers]]
name  = "Authorization"
value = "$http_authorization"

# ─── Content Replacement (sub_filter) ────────────────────────────
[[sites.locations.sub_filter]]
pattern     = "http://old.example.com"
replacement = "https://new.example.com"

# ─── Reverse Proxy Cookie Handling ───────────────────────────────
strip_cookie_secure  = false
proxy_cookie_domain  = "backend.internal example.com"

# ─── Reverse Proxy Redirect Handling ─────────────────────────────
proxy_redirect_from = "http://backend.internal/"
proxy_redirect_to   = "https://example.com/"

# ─── Buffering Control ───────────────────────────────────────────
proxy_buffering = false   # Disable buffering (set false for SSE/streaming)
```

## Supported Variables

The following variables can be used in `value`, `return_url`, `return_body`, etc.:

| Variable | Description |
|----------|-------------|
| `$remote_addr` | Client IP |
| `$host` | Request Host header |
| `$scheme` | Request protocol (http/https) |
| `$request_uri` | Full request path (with query string) |
| `$uri` | Request path (without query string) |
| `$args` | Query string |
| `$http_<name>` | Request header, e.g. `$http_authorization` |

## Common Examples

### Static Files with Long Cache

```toml
[[sites.locations]]
path    = "~* \\.(js|css|png|jpg|gif|ico|woff2?)$"
handler = "static"

[[sites.locations.cache_rules]]
pattern       = ".*"
cache_control = "public, max-age=2592000, immutable"
```

### PHP Full-Site Forwarding

```toml
[[sites.locations]]
path      = "~ \\.php$"
handler   = "fastcgi"
try_files = ["$uri", "=404"]
```

### Health Check Endpoint

```toml
[[sites.locations]]
path        = "= /health"
handler     = "static"
return_code = 200
return_body = "OK"
```

### Force CORS Headers

```toml
[[sites.locations]]
path    = "/api/"
handler = "reverse_proxy"
upstream = "backend"

[[sites.locations.add_headers]]
name  = "Access-Control-Allow-Origin"
value = "*"

[[sites.locations.add_headers]]
name  = "Access-Control-Allow-Methods"
value = "GET, POST, PUT, DELETE, OPTIONS"
```
