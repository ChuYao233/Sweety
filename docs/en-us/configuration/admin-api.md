# Admin API

Sweety ships with a full-featured REST Admin API that matches all Caddy Admin API capabilities and extends beyond them.
The API runs as an independent TCP listener and has zero impact on main server performance.

## Enabling

```toml
[global]
admin_listen = "127.0.0.1:9099"   # Listen address (empty = disabled)
admin_token  = "your-secret-token" # Bearer Token (empty = no auth)
```

> ⚠️ **Security**: Only bind to `127.0.0.1`. **Never expose to the public internet.**

## Authentication

When `admin_token` is set, all requests require `Authorization: Bearer <token>` except:

- `GET /api/health`
- `GET /health`
- `GET /api/version`
- `GET /api/doc`
- `GET /metrics`

## Core Concepts

### Runtime Config vs Disk Config

- **All changes only affect runtime memory by default** — they are lost on restart
- `GET /config` always returns the **currently running** config, not the disk file
- Append `?save=true` to also persist changes to the config file (TOML format)
- `POST /config/save` explicitly saves the current runtime config to disk at any time

```bash
# Runtime-only change (lost on restart)
curl -X PATCH http://127.0.0.1:9099/config/global \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"keepalive_timeout": 120}'

# Change + persist to config file
curl -X PATCH "http://127.0.0.1:9099/config/global?save=true" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"keepalive_timeout": 120}'

# Explicitly save running config to disk
curl -X POST http://127.0.0.1:9099/config/save \
  -H "Authorization: Bearer $TOKEN"
```

---

## Endpoint Reference

### Config Tree CRUD (Caddy `/config/` equivalent)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/load` | Hot-load full JSON config (auto-rollback on failure) |
| `GET` | `/config/[path]` | Read running config subtree |
| `POST` | `/config/[path]` | Create/replace object \| append to array |
| `PUT` | `/config/[path]` | Insert at array index \| strict create (error if exists) |
| `PATCH` | `/config/[path]` | Replace existing value only |
| `DELETE` | `/config/[path]` | Delete node (`/config/` = clear config, keep running) |
| `POST` | `/config/save` | Explicitly save running config to disk (TOML) |
| `POST` | `/config/reload` | Hot-reload config from disk |
| `POST` | `/config/test` | Validate disk config file syntax |

All write operations support `?save=true` query parameter to persist to config file.

### @id Node Access (Caddy `/id/` equivalent)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/id/:id` | Access config node by `@id` |
| `GET` | `/id/:id/[path]` | Access via `@id` + sub-path |

### Config Adapter (Caddy `/adapt` equivalent)

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/adapt` | TOML → JSON conversion (does not load) |

### Runtime Status (Caddy equivalents)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/reverse_proxy/upstreams` | Upstream status (Caddy-compatible JSON) |
| `GET` | `/metrics` | Prometheus text/plain metrics |
| `POST` | `/api/stop` | Graceful shutdown |

### System

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/health` | Health check (no auth) |
| `GET` | `/api/version` | Version + build info (no auth) |
| `GET` | `/api/system` | System info (uptime / workers / memory) |
| `GET` | `/api/doc` | API documentation JSON (no auth) |
| `GET` | `/api/debug` | Runtime debug info |
| `GET` | `/api/stats` | Global statistics snapshot |

### Sites

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/sites` | Site list + summary |
| `GET` | `/api/sites/:name` | Single site details |
| `DELETE` | `/api/sites/:name` | Delete site (takes effect immediately) |

### Upstreams

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/upstreams` | All upstream groups + node status |
| `GET` | `/api/upstreams/:name` | Single upstream group details |
| `POST` | `/api/upstreams/:name/nodes/:addr/enable` | Enable node |
| `POST` | `/api/upstreams/:name/nodes/:addr/disable` | Disable node |
| `PUT` | `/api/upstreams/:name/nodes/:addr/weight` | Update node weight |

### Certificates

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/certs` | TLS certificate list |
| `POST` | `/api/certs/reload` | Reload certificates from disk |
| `POST` | `/api/certs/acme/renew` | Trigger immediate ACME renewal (`?site=name` for specific site) |

### Cache

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/cache/stats` | Cache hit rate statistics |
| `POST` | `/api/cache/purge` | Purge all caches |

### Connections / Plugins / Logs

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/connections` | Active connections + pool status |
| `GET` | `/api/plugins` | Registered plugin list |
| `GET` | `/api/logs/level` | Current log level |
| `PUT` | `/api/logs/level` | Change log level |

---

## Usage Examples

### Health Check

```bash
curl http://127.0.0.1:9099/api/health
# {"status":"ok"}
```

### View Running Config

```bash
# Full config
curl http://127.0.0.1:9099/config/ -H "Authorization: Bearer $TOKEN"

# Global section only
curl http://127.0.0.1:9099/config/global -H "Authorization: Bearer $TOKEN"

# First site (by array index)
curl http://127.0.0.1:9099/config/sites/0 -H "Authorization: Bearer $TOKEN"
```

### Modify Global Config

```bash
# Runtime-only
curl -X PATCH http://127.0.0.1:9099/config/global \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"keepalive_timeout": 120}'

# Persist to disk
curl -X PATCH "http://127.0.0.1:9099/config/global?save=true" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"keepalive_timeout": 120}'
```

### Hot-Load Full Config

```bash
curl -X POST http://127.0.0.1:9099/load \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d @new-config.json
```

Automatically rolls back to the previous config on failure.

### Add a Site

```bash
curl -X POST http://127.0.0.1:9099/config/sites \
  -H "Authorization: Bearer $TOKEN" \
  -d '{
    "name": "new-site",
    "server_name": ["new.example.com"],
    "listen": [80],
    "root": "/var/www/new-site",
    "locations": [{"path": "/", "handler": "static"}]
  }'
```

### Upstream Node Control

```bash
# Drain node
curl -X POST http://127.0.0.1:9099/api/upstreams/backend/nodes/127.0.0.1%3A8080/disable \
  -H "Authorization: Bearer $TOKEN"

# Re-enable node
curl -X POST http://127.0.0.1:9099/api/upstreams/backend/nodes/127.0.0.1%3A8080/enable \
  -H "Authorization: Bearer $TOKEN"

# Change weight
curl -X PUT http://127.0.0.1:9099/api/upstreams/backend/nodes/127.0.0.1%3A8080/weight \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"weight": 5}'
```

### TOML → JSON Adapter

```bash
curl -X POST http://127.0.0.1:9099/adapt \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: text/plain" \
  --data-binary @sweety.toml
```

### Prometheus Metrics

```bash
curl http://127.0.0.1:9099/metrics
```

Available metrics:

| Metric | Type | Description |
|--------|------|-------------|
| `sweety_requests_total` | counter | Total requests processed |
| `sweety_active_requests` | gauge | Current concurrent requests |
| `sweety_errors_4xx_total` | counter | Total 4xx errors |
| `sweety_errors_5xx_total` | counter | Total 5xx errors |
| `sweety_bytes_sent_total` | counter | Total response bytes sent |
| `sweety_websocket_connections` | gauge | Current active WebSocket connections |

With analysis reports enabled: `sweety_avg_response_ms`, `sweety_error_rate_5xx`, `sweety_status_total{code="..."}`, etc.

### Stats Snapshot (/api/stats)

```bash
curl http://127.0.0.1:9099/api/stats -H "Authorization: Bearer $TOKEN"
```

Returns JSON:

```json
{
  "total_requests": 12345,
  "total_errors_4xx": 23,
  "total_errors_5xx": 2,
  "total_bytes_sent": 1048576,
  "active_requests": 5,
  "active_ws_connections": 1
}
```

---

## Comparison with Caddy Admin API

| Feature | Caddy | Sweety |
|---------|-------|--------|
| Config tree CRUD `/config/[path]` | ✅ | ✅ |
| Hot-load `/load` | ✅ | ✅ + auto-rollback |
| Clear config `DELETE /config/` | ✅ | ✅ |
| `@id` access `/id/:id` | ✅ | ✅ |
| Config adapter `/adapt` | ✅ Caddyfile→JSON | ✅ TOML→JSON |
| Upstream status `/reverse_proxy/upstreams` | ✅ | ✅ |
| Prometheus `/metrics` | ✅ | ✅ text/plain |
| Graceful stop `/stop` | ✅ | ✅ |
| Optional persistence `?save=true` | ✗ (always persists) | ✅ |
| Explicit save `/config/save` | ✗ | ✅ |
| Reload from disk `/config/reload` | ✗ | ✅ |
| Config validation `/config/test` | ✗ | ✅ |
| Site CRUD `/api/sites` | ✗ | ✅ |
| Node control enable/disable/weight | ✗ | ✅ |
| Certificate management `/api/certs` | ✗ | ✅ |
| Cache management `/api/cache` | ✗ | ✅ |
| Runtime debug `/api/debug` | `/debug/pprof` | ✅ |
| Log level toggle `/api/logs/level` | ✗ | ✅ |
| Plugin list `/api/plugins` | ✗ | ✅ |
| Bearer Token auth | Mutual TLS | ✅ |
| ACME instant renewal `/api/certs/acme/renew` | ✗ | ✅ async + SAN multi-domain |
| CORS support | ✗ | ✅ |
| API doc endpoint `/api/doc` | ✗ | ✅ |

---

## CLI Integration

```bash
# Print API doc JSON
sweety --api-doc

# Trigger hot-reload (equivalent to POST /config/reload)
sweety reload

# Validate config file
sweety validate -c sweety.toml
```
