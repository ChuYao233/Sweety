# Hot Reload

Hot reload allows reloading the configuration file **without dropping existing connections**, equivalent to `nginx -s reload`.

## Prerequisites

Admin API listen address must be configured:

```toml
[global]
admin_listen = "127.0.0.1:9099"
admin_token  = "your-secret-token"   # Optional, recommended for production
```

## Trigger Hot Reload

```bash
sweety reload
sweety reload -c /etc/sweety/sweety.toml
```

Equivalent API call:

```bash
curl -X POST \
  -H "Authorization: Bearer your-secret-token" \
  http://127.0.0.1:9099/api/reload
```

## Hot Reload Scope

| Config Item | Hot Reload Support |
|-------------|-------------------|
| Site `server_name` / `root` / `index` | ✅ |
| `[[sites.locations]]` routing rules | ✅ |
| `[[sites.upstreams]]` upstream nodes | ✅ |
| `[sites.fastcgi]` FastCGI config | ✅ |
| `[sites.rate_limit]` rate limit rules | ✅ |
| `[sites.proxy_cache]` cache config | ✅ |
| `[global]` log level | ✅ |
| Listen ports (`listen` / `listen_tls`) | ⚠️ Requires restart |
| TLS certificate file paths | ⚠️ Requires restart |
| `[global] worker_threads` | ⚠️ Requires restart |

## systemd Integration

Configure `ExecReload` in the systemd unit file so `systemctl reload sweety` triggers hot reload:

```ini
[Service]
ExecStart  = /usr/local/bin/sweety run -c /etc/sweety/sweety.toml
ExecReload = /usr/local/bin/sweety reload -c /etc/sweety/sweety.toml
```

```bash
# Reload config (no connection drops)
sudo systemctl reload sweety

# Full restart (drops all connections)
sudo systemctl restart sweety
```

## Config Change Workflow

```bash
# 1. Edit config
vim /etc/sweety/sweety.toml

# 2. Validate syntax
sweety validate -c /etc/sweety/sweety.toml

# 3. Hot reload (zero downtime)
sweety reload -c /etc/sweety/sweety.toml

# Or one step
sweety validate -c /etc/sweety/sweety.toml && sweety reload -c /etc/sweety/sweety.toml
```

## Monitor Reload Results

After reload, confirm the config has been updated via the API:

```bash
curl -H "Authorization: Bearer your-secret-token" \
  http://127.0.0.1:9099/api/status
```

The logs will show:

```
INFO sweety::config::hot_reload: Config hot reload complete, sites: 3
```
