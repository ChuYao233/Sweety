# Configuration Overview

## Format

Sweety supports three configuration formats, auto-detected by file extension:

| Extension | Format |
|-----------|--------|
| `.toml` | TOML (recommended) |
| `.json` | JSON |
| `.yaml` / `.yml` | YAML |

## Structure

```toml
# Global config (optional, has sensible defaults)
[global]
worker_threads = 0
log_level      = "info"
# ...

# Site list (one or more)
[[sites]]
name        = "site1"
server_name = ["example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/site1"

  # Site-level TLS config
  [sites.tls]
  acme       = true
  acme_email = "your@email.com"

  # Site-level FastCGI config
  [sites.fastcgi]
  socket = "/run/php/php8.2-fpm.sock"

  # Routing rules (multiple allowed)
  [[sites.locations]]
  path    = "/"
  handler = "php"

  # Upstream server groups (for reverse proxy)
  [[sites.upstreams]]
  name  = "backend"
  nodes = [{ addr = "127.0.0.1:3000" }]
```

## Minimal Config

### Static Site (HTTP only)

```toml
[[sites]]
name        = "static"
server_name = ["example.com"]
listen      = [80]
root        = "/var/www/html"
```

### Auto-HTTPS Static Site

```toml
[[sites]]
name        = "static"
server_name = ["example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/html"
acme_email  = "your@email.com"
```

### WordPress (Out of the Box)

```toml
[[sites]]
name        = "wp"
server_name = ["example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/wordpress"
preset      = "wordpress"
php_fastcgi = "/run/php/php8.2-fpm.sock"
acme_email  = "your@email.com"
```

## Config Loading Flow

```
Read file
   ↓
TOML/JSON/YAML parse
   ↓
expand_config()        ← Expand sugar fields (preset, php_fastcgi, acme_email)
   ↓
validate_config()      ← Validate required fields, cert paths, port conflicts
   ↓
Start server
```

## Config File Path

Default path: `config/sweety.toml`

Override via environment variable:

```bash
SWEETY_CONFIG=/etc/sweety/sweety.toml sweety run
```

Override via command line:

```bash
sweety run -c /etc/sweety/sweety.toml
```
