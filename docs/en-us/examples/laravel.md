# Example: Laravel

## Minimal Configuration (Recommended)

```toml
[[sites]]
name        = "laravel"
server_name = ["app.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/laravel/public"
preset      = "laravel"
php_fastcgi = "/run/php/php8.2-fpm.sock"
acme_email  = "your@email.com"
```

> **Note**: `root` points to Laravel's `public` subdirectory, not the project root.

---

## Full Configuration

```toml
[[sites]]
name        = "laravel"
server_name = ["app.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/laravel/public"
force_https = true
gzip        = true
acme_email  = "your@email.com"

[sites.fastcgi]
socket      = "/run/php/php8.2-fpm.sock"
pool_size   = 32
read_timeout = 60

[sites.hsts]
max_age = 31536000

# Block .env and other sensitive files
[[sites.locations]]
path        = "~ /\\.(env|git|htaccess)$"
handler     = "static"
return_code = 403

# Static assets with long cache (Vite/Mix build artifacts with hash, permanent cache)
[[sites.locations]]
path    = "~* \\.(js|css|png|jpg|jpeg|gif|ico|svg|woff|woff2|ttf|eot|webp)$"
handler = "static"

[[sites.locations.cache_rules]]
pattern       = ".*"
cache_control = "public, max-age=2592000, immutable"

# PHP files
[[sites.locations]]
path      = "~ \\.php$"
handler   = "fastcgi"
try_files = ["$uri", "=404"]

# Laravel routing (all requests to index.php)
[[sites.locations]]
path      = "/"
handler   = "static"
try_files = ["$uri", "$uri/", "/index.php?$query_string"]
```

---

## Laravel Octane (Swoole/RoadRunner)

Laravel Octane starts a long-running process — no PHP-FPM needed, use reverse proxy directly:

```toml
[[sites]]
name        = "laravel-octane"
server_name = ["app.example.com"]
listen      = [80]
listen_tls  = [443]
acme_email  = "your@email.com"

[[sites.upstreams]]
name  = "octane"
nodes = [{ addr = "127.0.0.1:8000" }]

[[sites.locations]]
path     = "/"
handler  = "reverse_proxy"
upstream = "octane"

[[sites.locations.proxy_set_headers]]
name  = "X-Real-IP"
value = "$remote_addr"

[[sites.locations.proxy_set_headers]]
name  = "X-Forwarded-Proto"
value = "$scheme"
```

---

## API Site (Pure JSON API)

```toml
[[sites]]
name        = "laravel-api"
server_name = ["api.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/laravel-api/public"
force_https = true
acme_email  = "your@email.com"

[sites.fastcgi]
socket = "/run/php/php8.2-fpm.sock"

# Rate limiting (API abuse prevention)
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension = "ip"
rate      = 60
burst     = 120

# PHP files
[[sites.locations]]
path      = "~ \\.php$"
handler   = "fastcgi"
try_files = ["$uri", "=404"]

# Laravel routing
[[sites.locations]]
path      = "/"
handler   = "static"
try_files = ["$uri", "$uri/", "/index.php?$query_string"]
```

---

## Directory Permissions

```bash
# Ensure storage and bootstrap/cache are writable
chown -R www-data:www-data /var/www/laravel/storage
chown -R www-data:www-data /var/www/laravel/bootstrap/cache
chmod -R 775 /var/www/laravel/storage
chmod -R 775 /var/www/laravel/bootstrap/cache
```
