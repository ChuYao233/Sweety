# Configuration Reference

## Global

| Field | Default | Description |
|-------|---------|-------------|
| `worker_threads` | 0 (= CPU count) | Worker thread count |
| `worker_connections` | 51200 | Max concurrent connections |
| `max_connections` | 0 (unlimited) | Global connection limit, 503 when exceeded |
| `keepalive_timeout` | 60 | Keep-alive timeout (seconds) |
| `client_max_body_size` | 50 | Max request body (MB) |
| `client_header_buffer_size` | 32 | Request header buffer (KB) |
| `gzip` | false | Global gzip compression |
| `gzip_min_length` | 1 | Min size to compress (KB) |
| `gzip_comp_level` | 5 | Compression level 1-9 |
| `admin_listen` | — | Admin API listen address |
| `admin_token` | — | Admin API Bearer token |
| `log_level` | — | tracing filter (e.g. `sweety_lib=info`) |

## Sites

| Field | Description |
|-------|-------------|
| `name` | Site identifier |
| `server_name` | Hostnames (exact / `*.wildcard` / `fallback = true`) |
| `listen` | HTTP ports |
| `listen_tls` | HTTPS ports |
| `root` | Document root |
| `index` | Index files |
| `access_log` | Log file path |
| `access_log_format` | `combined` / `json` / custom template |
| `force_https` | 301 redirect to HTTPS |
| `fallback` | Default site when no SNI match |
| `gzip` | Override global gzip |
| `websocket` | Allow WebSocket upgrades |
| `error_pages` | `{404 = "404.html"}` |

## TLS (`[sites.tls]`)

| Field | Description |
|-------|-------------|
| `acme` | Enable ACME auto-cert |
| `acme_email` | ACME account email |
| `acme_provider` | `letsencrypt` / `zerossl` / `buypass` |
| `acme_challenge` | `http01` / `dns01` |
| `acme_renew_days_before` | Renew N days before expiry |
| `certs[]` | `{cert = "...", key = "..."}` static certs |
| `min_version` / `max_version` | `tls1.2` / `tls1.3` |
| `protocols` | Enabled HTTP protocols in priority order: `["h3", "h2", "http/1.1"]` (default all) |

## Upstreams (`[[sites.upstreams]]`)

| Field | Default | Description |
|-------|---------|-------------|
| `name` | — | Upstream group name |
| `strategy` | `round_robin` | `round_robin` / `weighted` / `least_conn` / `ip_hash` |
| `connect_timeout` | 10 | Connect timeout (seconds) |
| `read_timeout` | 60 | Read timeout (seconds) |
| `write_timeout` | 60 | Write timeout (seconds) |
| `retry` | 0 | Retry count on failure |
| `retry_timeout` | 0 | Wait before retry (seconds) |
| `keepalive` | 32 | Max idle connections in pool |
| `keepalive_requests` | 1000 | Max requests per connection |
| `keepalive_time` | 60 | Max connection age (seconds) |

### Circuit Breaker (`[sites.upstreams.circuit_breaker]`)

| Field | Default | Description |
|-------|---------|-------------|
| `max_failures` | 5 | Failures in window to open circuit |
| `window_secs` | 60 | Sliding window (seconds) |
| `fail_timeout` | 30 | Half-open probe interval (seconds) |

### Nodes (`[[sites.upstreams.nodes]]`)

| Field | Description |
|-------|-------------|
| `addr` | `host:port` |
| `weight` | Load balancing weight |
| `tls` | Enable TLS to upstream |
| `tls_sni` | TLS SNI hostname |
| `tls_insecure` | Skip upstream cert verification |
| `upstream_host` | Override Host header sent to upstream |

## Locations (`[[sites.locations]]`)

| Field | Description |
|-------|-------------|
| `path` | Match pattern (`/`, `= /exact`, `^~ /prefix`, `~ regex`) |
| `handler` | `static` / `reverse_proxy` / `fastcgi` / `grpc` / `websocket` / `plugin:xxx` |
| `upstream` | Upstream group name (reverse_proxy/grpc) |
| `root` | Override site root |
| `try_files` | `["$uri", "$uri/", "/index.php"]` |
| `return_code` | Return HTTP status code directly |
| `return_url` | `"301 https://..."` redirect |
| `return_body` | Return text body directly |
| `return_content_type` | Content-Type for return_body |
| `limit_conn` | Max concurrent connections to this location |
| `proxy_buffering` | Buffer upstream response (default true) |
| `auth_request` | Sub-request auth URL |
| `auth_failure_status` | Status code on auth failure (default 401) |
| `proxy_set_headers[]` | `{name, value}` — override request headers to upstream |
| `add_headers[]` | `{name, value}` — inject response headers |
| `sub_filter[]` | `{pattern, replacement}` — response body rewrite |
| `cache_rules[]` | `{pattern, cache_control}` — path-based cache headers |
| `strip_cookie_secure` | Remove Secure flag from Set-Cookie |
| `proxy_cookie_domain` | Rewrite Set-Cookie Domain |
| `proxy_redirect_from/to` | Rewrite Location header |

## Access Log Format Variables

| Variable | Description |
|----------|-------------|
| `$remote_addr` | Client IP |
| `$method` | HTTP method |
| `$uri` | Request URI |
| `$status` | Response status |
| `$bytes_sent` | Response bytes |
| `$http_referer` | Referer header |
| `$http_user_agent` | User-Agent header |
| `$duration_ms` | Request duration (ms) |
| `$time_local` | Local time |
| `$site` | Site name |

Full example: [`config/sweety.example.toml`](../config/sweety.example.toml)
