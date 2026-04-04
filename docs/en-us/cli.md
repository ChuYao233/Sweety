# CLI Reference

## Syntax

```
sweety [OPTIONS] [COMMAND]
```

## Global Options

| Option | Short | Default | Description |
|--------|-------|---------|-------------|
| `--config <PATH>` | `-c` | `config/sweety.toml` | Config file path |
| `--pid-file <PATH>` | — | `/var/run/sweety.pid` | PID file path (daemon mode) |
| `--version` | `-v` | — | Show version |

Environment variable `SWEETY_CONFIG` can replace `-c`:

```bash
SWEETY_CONFIG=/etc/sweety/sweety.toml sweety run
```

---

## Subcommands

### `run` — Foreground

Run in foreground, recommended under systemd / supervisord:

```bash
sweety run
sweety run -c /etc/sweety/sweety.toml
```

- Logs output to stdout/stderr
- Graceful shutdown on `SIGTERM` / `SIGINT` (waits for existing connections)
- Default subcommand when omitted

---

### `start` — Background (Daemon)

Start in background, writes PID file:

```bash
sweety start
sweety start -c /etc/sweety/sweety.toml --pid-file /var/run/sweety.pid
```

---

### `stop` — Stop Background Process

Reads PID file, sends `SIGTERM`:

```bash
sweety stop
sweety stop --pid-file /var/run/sweety.pid
```

---

### `restart` — Restart

Equivalent to `stop` + `start`:

```bash
sweety restart
sweety restart -c /etc/sweety/sweety.toml
```

---

### `reload` — Hot Reload Config

Reload config without dropping existing connections:

```bash
sweety reload
sweety reload -c /etc/sweety/sweety.toml
```

**Prerequisite**: `global.admin_listen` must be configured — reload sends a signal via the Admin API.

```toml
[global]
admin_listen = "127.0.0.1:9099"
```

Hot reload scope:
- ✅ Site config (server_name, root, locations, etc.)
- ✅ Rate limiting, cache config
- ✅ Upstream node list
- ⚠️ Listen port changes require restart

---

### `validate` — Validate Config

Equivalent to `nginx -t`, checks config syntax and TLS certificates:

```bash
sweety validate
sweety validate -c /etc/sweety/sweety.toml
```

Checks:
- TOML/JSON/YAML syntax validity
- Required fields (`name`, `server_name`)
- TLS certificate and key file paths are readable
- Upstream node format correctness
- Port conflict detection

Success output:

```
Configuration check passed ✓
  Sites: 3
  Listen ports: 80, 443
  TLS sites: 2 (ACME: 1, Manual: 1)
```

---

### `version` — Show Version

```bash
sweety version
# or
sweety -v
```

---

### `api-doc` — Output Admin API Docs

Output Admin REST API documentation (OpenAPI format):

```bash
sweety api-doc
sweety api-doc > api-doc.json
```

---

## Admin API

After configuring `admin_listen`, Sweety provides a REST API:

```toml
[global]
admin_listen = "127.0.0.1:9099"
admin_token  = "your-secret-token"
```

### Authentication

All API requests require a Bearer Token:

```bash
curl -H "Authorization: Bearer your-secret-token" http://127.0.0.1:9099/api/status
```

### Main Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/status` | Server status (version, uptime, connections) |
| `GET` | `/api/sites` | All site config summaries |
| `POST` | `/api/reload` | Trigger hot reload (equivalent to `sweety reload`) |
| `GET` | `/metrics` | Prometheus metrics (no auth required) |

### Prometheus Metrics

```bash
curl http://127.0.0.1:9099/metrics
```

Metrics include:
- `sweety_requests_total` — Total requests (by site, status code)
- `sweety_active_connections` — Active connections
- `sweety_request_duration_seconds` — Request duration distribution
- `sweety_upstream_errors_total` — Upstream errors
- `sweety_cache_hits_total` — Cache hits

---

## Common Operations

```bash
# Install as systemd service (foreground mode, recommended)
sudo systemctl start sweety

# Reload config (no service interruption)
sweety reload -c /etc/sweety/sweety.toml

# Test config then restart
sweety validate -c /etc/sweety/sweety.toml && sweety restart -c /etc/sweety/sweety.toml
```
