# Global Configuration [global]

Global configuration affects all sites. All fields have default values — the `[global]` block can be omitted entirely.

## Full Configuration

```toml
[global]
# ─── Threads & Connections ────────────────────────────────────────
worker_threads     = 0       # Worker threads, 0 = auto (CPU core count)
worker_connections = 51200   # Max concurrent connections per worker
max_connections    = 0       # Global max connections, 0 = unlimited
keepalive_timeout  = 60      # Keep-Alive timeout (seconds)

# ─── Request Limits ──────────────────────────────────────────────
client_max_body_size       = 50    # Max request body (MB)
client_header_buffer_size  = 32    # Request header buffer (KB)
client_body_buffer_size    = 512   # Request body buffer (KB)

# ─── FastCGI Global Default Timeouts ─────────────────────────────
fastcgi_connect_timeout = 5    # Connect timeout (seconds)
fastcgi_read_timeout    = 60   # Read timeout (seconds)

# ─── Compression ─────────────────────────────────────────────────
gzip            = false  # Enable gzip globally
gzip_min_length = 1      # Min compression size (KB)
gzip_comp_level = 5      # Compression level 1-9

# ─── HTTP/2 ──────────────────────────────────────────────────────
h2_max_concurrent_streams       = 128   # Max concurrent streams per connection
h2_max_pending_per_conn         = 0     # Max queued requests (0 = unlimited)
h2_max_concurrent_reset_streams = 200   # RST flood protection
h2_max_frame_size               = 65535 # Max frame size (bytes)
h2_max_requests_per_conn        = 1000  # Max requests per connection (0 = unlimited)

# ─── HTTP/3 ──────────────────────────────────────────────────────
h3_max_handlers = 0   # Global max concurrent H3 handlers (0 = auto, 80% available memory / 2MB)

# ─── Logging ─────────────────────────────────────────────────────
log_level = "info"      # error / warn / info / debug / trace
error_log = "/var/log/sweety/error.log"  # Error log path (optional)

# ─── Admin API ───────────────────────────────────────────────────
admin_listen = "127.0.0.1:9099"   # Admin API listen address (empty = disabled)
admin_token  = "your-secret-token" # Bearer Token auth

# ─── Prometheus Metrics ──────────────────────────────────────────
prometheus_enabled = true
prometheus_path    = "/metrics"    # Mounted on admin_listen
```

## Field Reference

### Threads & Connections

| Field | Default | Description |
|-------|---------|-------------|
| `worker_threads` | `0` | `0` = auto CPU core count, equivalent to `nginx worker_processes auto` |
| `worker_connections` | `51200` | Equivalent to `nginx worker_connections` |
| `max_connections` | `0` | Total concurrent connection limit, `0` = unlimited |
| `keepalive_timeout` | `60` | TCP Keep-Alive timeout, `0` = disabled |

### Request Limits

| Field | Default | Description |
|-------|---------|-------------|
| `client_max_body_size` | `50` MB | Returns `413` when exceeded, equivalent to `nginx client_max_body_size` |
| `client_header_buffer_size` | `32` KB | Request header buffer |
| `client_body_buffer_size` | `512` KB | Request body buffer |

### HTTP/2

| Field | Default | Description |
|-------|---------|-------------|
| `h2_max_concurrent_streams` | `128` | Max concurrent requests per connection, equivalent to `nginx http2_max_concurrent_streams` |
| `h2_max_pending_per_conn` | `0` | Max in-flight handlers per connection, `0` = unlimited. Limiting concurrent handlers reduces peak memory |
| `h2_max_concurrent_reset_streams` | `200` | RST flood protection (CVE-2023-44487) |
| `h2_max_frame_size` | `65535` | HTTP/2 frame size, affects large file transfer efficiency |
| `h2_max_requests_per_conn` | `1000` | Connection reuse limit, closes after exceeding, `0` = unlimited |

### HTTP/3

| Field | Default | Description |
|-------|---------|-------------|
| `h3_max_handlers` | `0` | Global max concurrent H3 handlers. `0` = auto (80% available system memory / 2MB). Each QUIC connection buffers up to `send_window` bytes; this limit prevents OOM. Excess connections are queued, not rejected |

### Admin API

After configuring `admin_listen`, the REST API (`/api/v1/*`) becomes available:

**Implemented:**
- `GET /api/v1/health` — Health check
- `GET /api/v1/version` — Version info
- `GET /api/v1/stats` — Global request statistics snapshot
- `GET /api/v1/plugins` — Registered plugin list
- `GET /api/v1/doc` — API documentation (JSON)
- Bearer Token authentication

**Planned for v0.5:**
- Site management, upstream node control, API hot reload
- WebSocket real-time stats push
- Prometheus `/metrics` endpoint

Security recommendation: Only listen on `127.0.0.1`, **never expose to the public internet**.
