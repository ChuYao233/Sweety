# Quick Start

## Launch a WordPress Site in 5 Minutes

### 1. Create Config Directory

```bash
mkdir -p /etc/sweety
```

### 2. Write Minimal Config

`/etc/sweety/sweety.toml`:

```toml
[[sites]]
name        = "wordpress"
server_name = ["php.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/wordpress"
preset      = "wordpress"
php_fastcgi = "/run/php/php8.2-fpm.sock"
acme_email  = "your@email.com"
```

These 8 lines are equivalent to 50+ lines of Nginx config, automatically providing:
- HTTP → HTTPS 301 redirect
- Let's Encrypt automatic certificate issuance and renewal
- Optimal WordPress location routing rules (static files served directly, PHP forwarding, security filtering)
- HTTP/1.1 + HTTP/2 + HTTP/3 full protocol support

### 3. Validate Config

```bash
sweety validate -c /etc/sweety/sweety.toml
```

### 4. Start

```bash
# Foreground (for debugging)
sweety run -c /etc/sweety/sweety.toml

# Background
sweety start -c /etc/sweety/sweety.toml
```

---

## Launch a Static Site in 5 Minutes

```toml
[[sites]]
name        = "blog"
server_name = ["blog.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/blog"
preset      = "static"
acme_email  = "your@email.com"
```

---

## Configure a Reverse Proxy in 5 Minutes

```toml
[[sites]]
name        = "api"
server_name = ["api.example.com"]
listen      = [80]
listen_tls  = [443]
acme_email  = "your@email.com"

[[sites.upstreams]]
name  = "backend"
nodes = [
    { addr = "127.0.0.1:3000", weight = 1 }
]

[[sites.locations]]
path    = "/"
handler = "proxy"
upstream = "backend"
```

---

## Multi-Site Example

```toml
[[sites]]
name        = "site1"
server_name = ["site1.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/site1"
preset      = "static"
acme_email  = "your@email.com"

[[sites]]
name        = "site2"
server_name = ["site2.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/wordpress"
preset      = "wordpress"
php_fastcgi = "/run/php/php8.2-fpm.sock"
acme_email  = "your@email.com"
```

Multiple sites share the same port — Sweety automatically routes via SNI (TLS) and `Host` header (HTTP).
