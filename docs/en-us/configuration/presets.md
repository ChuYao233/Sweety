# Built-in Presets

The `preset` field applies optimal location rules with a single line, equivalent to writing dozens of `[[sites.locations]]` entries manually.

## Usage

```toml
[[sites]]
preset = "wordpress"   # wordpress / laravel / static
```

> **Manual takes priority**: If `[[sites.locations]]` already exists, `preset` has no effect.

---

## wordpress

Optimal WordPress configuration, includes:

- Long cache for static assets (JS/CSS/images/fonts)
- PHP files forwarded to FastCGI
- WordPress permalinks (`/index.php?$args`)
- Block access to sensitive files (`.php` in `/wp-content/uploads/`, `xmlrpc.php`, `.htaccess`)
- Block malicious User-Agents

Equivalent manual configuration:

```toml
# Block PHP files in wp-content/uploads (prevent Webshell)
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

# PHP files → FastCGI
[[sites.locations]]
path     = "~ \\.php$"
handler  = "fastcgi"
try_files = ["$uri", "=404"]

# WordPress permalinks (route all URLs to index.php)
[[sites.locations]]
path      = "/"
handler   = "static"
try_files = ["$uri", "$uri/", "/index.php?$args"]
```

### Usage Example

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

[sites.fastcgi.cache]
max_entries    = 2000
ttl            = 300
ignore_headers = ["Cache-Control", "Set-Cookie"]
```

---

## laravel

Optimal Laravel framework configuration, includes:

- Long cache for static assets
- All requests routed to `public/index.php` (Laravel standard entry point)
- Block access to `.env`, `.git`, and other sensitive directories

### Usage Example

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

---

## static

Optimal pure static site configuration, includes:

- Long cache for static assets (JS/CSS/images/fonts)
- `try_files $uri $uri/ =404` (standard static file behavior)
- Automatic compression negotiation (gzip/brotli)

### Usage Example

```toml
[[sites]]
name        = "blog"
server_name = ["blog.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/blog/public"
preset      = "static"
acme_email  = "your@email.com"
```

---

## Extending Presets (Adding Rules on Top)

`preset` expands by inserting preset rules at the **beginning** of the `locations` list. To add custom rules, simply write them manually:

```toml
[[sites]]
preset      = "wordpress"
php_fastcgi = "/run/php/php8.2-fpm.sock"

# Manually add rate limiting (effective alongside preset rules)
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension = "ip"
rate      = 60
burst      = 100
```

To **fully customize** locations, omit `preset` and write `[[sites.locations]]` manually.
