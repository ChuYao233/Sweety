# Example: WordPress

## Minimal Configuration (Recommended)

```toml
[[sites]]
name        = "wordpress"
server_name = ["blog.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/wordpress"
preset      = "wordpress"
php_fastcgi = "/run/php/php8.2-fpm.sock"
acme_email  = "your@email.com"
```

8 lines of config automatically include: ACME auto HTTPS, HTTP→HTTPS redirect, WordPress permalinks, PHP forwarding, static asset caching, and security filtering rules.

---

## Full Configuration with Caching

```toml
[[sites]]
name        = "wordpress"
server_name = ["blog.example.com", "www.blog.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/wordpress"
preset      = "wordpress"
php_fastcgi = "/run/php/php8.2-fpm.sock"
acme_email  = "your@email.com"
force_https = true
gzip        = true

# FastCGI response cache
[sites.fastcgi.cache]
max_entries    = 2000
ttl            = 300
ignore_headers = ["Cache-Control", "Set-Cookie"]

# HSTS
[sites.hsts]
max_age            = 31536000
include_subdomains = true

# Rate limiting: brute force login protection
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension    = "ip"
rate         = 30
burst        = 60
path_pattern = "^/wp-login\\.php$"
```

---

## Multi-Site WordPress

```toml
# Site 1
[[sites]]
name        = "blog1"
server_name = ["blog1.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/blog1"
preset      = "wordpress"
php_fastcgi = "/run/php/php8.2-fpm.sock"
acme_email  = "your@email.com"

# Site 2 (different PHP version)
[[sites]]
name        = "blog2"
server_name = ["blog2.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/blog2"
preset      = "wordpress"
php_fastcgi = "/run/php/php8.0-fpm.sock"
acme_email  = "your@email.com"
```

---

## Full Manual Configuration (Without Preset)

```toml
[[sites]]
name        = "wordpress-manual"
server_name = ["blog.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/wordpress"
force_https = true

[sites.tls]
acme       = true
acme_email = "your@email.com"

[sites.fastcgi]
socket    = "/run/php/php8.2-fpm.sock"
pool_size = 32

[sites.fastcgi.cache]
max_entries    = 2000
ttl            = 300
ignore_headers = ["Cache-Control", "Set-Cookie"]

# Block PHP execution in wp-content/uploads (prevent Webshell)
[[sites.locations]]
path        = "~ /wp-content/uploads/.*\\.php$"
handler     = "static"
return_code = 403

# Block sensitive files
[[sites.locations]]
path        = "~ ^/(xmlrpc\\.php|\\.htaccess|\\.env)$"
handler     = "static"
return_code = 403

# Static assets with long cache
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

# WordPress permalinks
[[sites.locations]]
path      = "/"
handler   = "static"
try_files = ["$uri", "$uri/", "/index.php?$args"]
```

---

## Performance Tuning Tips

### PHP-FPM Configuration (`/etc/php/8.2/fpm/pool.d/www.conf`)

```ini
pm = dynamic
pm.max_children      = 50
pm.start_servers     = 10
pm.min_spare_servers = 5
pm.max_spare_servers = 20
pm.max_requests      = 500
```

Set `pool_size` to 70% of `pm.max_children` (i.e. 35):

```toml
[sites.fastcgi]
socket    = "/run/php/php8.2-fpm.sock"
pool_size = 35
```

### OPcache (`/etc/php/8.2/fpm/conf.d/10-opcache.ini`)

```ini
opcache.enable           = 1
opcache.memory_consumption = 256
opcache.max_accelerated_files = 20000
opcache.revalidate_freq  = 0
opcache.validate_timestamps = 0
```
