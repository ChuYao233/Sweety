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

## IP Access Control (access_rules)

Equivalent to Nginx `allow` / `deny` directives. Rules are matched in order against the client IP; the first match determines the result. Supports exact IP, CIDR ranges, and the `all` wildcard.

### Syntax

```toml
[[sites.locations]]
path    = "/admin"
handler = "reverse_proxy"
upstream = "backend"

# Only allow internal network access to admin panel
[[sites.locations.access_rules]]
action = "allow"
source = "10.0.0.0/8"

[[sites.locations.access_rules]]
action = "allow"
source = "172.16.0.0/12"

[[sites.locations.access_rules]]
action = "allow"
source = "192.168.0.0/16"

[[sites.locations.access_rules]]
action = "deny"
source = "all"
```

### Rule Reference

| Field | Default | Description |
|-------|---------|-------------|
| `action` | (required) | `allow` or `deny` |
| `source` | (required) | IP address, CIDR range, or `all` (matches everything) |
| `priority` | `0` | Priority (0-1024, lower value = higher priority) |

- Rules are sorted by `priority` ascending then matched top-to-bottom, **first match wins**
- Same-priority rules preserve their order in the config file
- No rules or no match **defaults to allow** (consistent with Nginx)
- Denied requests return **403 Forbidden**
- Supports IPv4 and IPv6
- Rules are pre-compiled to CIDR matchers at startup, zero allocation at runtime

### Typical Usage

```toml
# Whitelist mode: allow specific IPs, deny the rest
[[sites.locations.access_rules]]
action = "allow"
source = "1.2.3.4"

[[sites.locations.access_rules]]
action = "deny"
source = "all"

# Blacklist mode: deny specific IPs, allow the rest
[[sites.locations.access_rules]]
action = "deny"
source = "5.6.7.8"

[[sites.locations.access_rules]]
action = "allow"
source = "all"

# Priority mode: deny all at priority 10, allow internal at priority 0 (matched first)
[[sites.locations.access_rules]]
action   = "deny"
source   = "all"
priority = 10

[[sites.locations.access_rules]]
action   = "allow"
source   = "10.0.0.0/8"
priority = 0           # Higher priority, internal IPs match this rule first
```

> When `real_ip` is configured on the site, access control uses the extracted real client IP, not the connection IP.

## Rewrite Rules

`rewrite` rules perform regex matching and replacement on request paths, equivalent to Nginx's `rewrite` directive. Rules are evaluated in array order; after a match, the `flag` determines subsequent behavior.

### Syntax

```toml
[[sites.locations]]
path = "/"
handler = "reverse_proxy"
upstream = "backend"

[[sites.locations.rewrite]]
pattern   = "^/old/(.*)$"     # Regex pattern (supports capture groups)
target    = "/new/$1"          # Replacement target ($1..$9 reference capture groups)
flag      = "last"             # Behavior flag (default: last)
condition = "!-f"              # Optional trigger condition
```

### Capture Group Substitution

| Placeholder | Description |
|-------------|-------------|
| `$0` | Full match |
| `$1` .. `$9` | Capture groups 1 through 9 |

### Behavior Flags

| Flag | Description |
|------|-------------|
| `last` | Stop processing subsequent rewrite rules, re-match location (default) |
| `break` | Stop processing subsequent rewrite rules, no re-match |
| `redirect` | Return 302 temporary redirect |
| `permanent` | Return 301 permanent redirect |

### Conditions

| Condition | Description |
|-----------|-------------|
| `!-f` | Trigger when file does not exist |
| `!-d` | Trigger when directory does not exist |
| `-f` | Trigger when file exists |
| `-d` | Trigger when directory exists |

### Examples

#### WordPress Pretty Permalinks

```toml
[[sites.locations.rewrite]]
pattern   = "^/(.+)$"
target    = "/index.php?$1"
flag      = "last"
condition = "!-f"
```

#### 301 Permanent Redirect for Legacy Paths

```toml
[[sites.locations.rewrite]]
pattern = "^/blog/(.*)$"
target  = "/articles/$1"
flag    = "permanent"
```

#### Chained Rules

```toml
# Rule 1: Strip .html suffix
[[sites.locations.rewrite]]
pattern = "^(/.*)\\.html$"
target  = "$1"
flag    = "last"

# Rule 2: API version routing
[[sites.locations.rewrite]]
pattern = "^/api/v1/(.*)$"
target  = "/api/current/$1"
flag    = "break"
```
