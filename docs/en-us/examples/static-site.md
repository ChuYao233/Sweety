# Example: Static Site

## Minimal Configuration

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

## SPA (Single Page Application)

React / Vue / Angular SPAs need all routes redirected to `index.html`:

```toml
[[sites]]
name        = "spa"
server_name = ["app.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/spa/dist"
force_https = true
gzip        = true
acme_email  = "your@email.com"

# JS/CSS build artifacts (hashed, permanent cache)
[[sites.locations]]
path    = "~* \\.(js|css|woff2?|ttf|eot)$"
handler = "static"

[[sites.locations.cache_rules]]
pattern       = ".*"
cache_control = "public, max-age=31536000, immutable"

# Image assets with long cache
[[sites.locations]]
path    = "~* \\.(png|jpg|jpeg|gif|ico|svg|webp)$"
handler = "static"

[[sites.locations.cache_rules]]
pattern       = ".*"
cache_control = "public, max-age=2592000"

# SPA route fallback (all unmatched paths return index.html)
[[sites.locations]]
path      = "/"
handler   = "static"
try_files = ["$uri", "$uri/", "/index.html"]
```

---

## Static Documentation Site (Jekyll/Hugo/VitePress)

```toml
[[sites]]
name        = "docs"
server_name = ["docs.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/docs/public"
force_https = true
gzip        = true
acme_email  = "your@email.com"

[sites.hsts]
max_age = 31536000

# Security headers
[[sites.locations]]
path    = "/"
handler = "static"
try_files = ["$uri", "$uri/", "$uri.html", "=404"]

[[sites.locations.add_headers]]
name  = "X-Content-Type-Options"
value = "nosniff"

[[sites.locations.add_headers]]
name  = "X-Frame-Options"
value = "SAMEORIGIN"

[[sites.locations.add_headers]]
name  = "Referrer-Policy"
value = "strict-origin-when-cross-origin"

# Static asset cache
[[sites.locations.cache_rules]]
pattern       = "\\.(js|css|woff2?|png|jpg|svg|ico)$"
cache_control = "public, max-age=86400"
```

---

## Static + CDN Origin

As a CDN origin server, add CORS headers:

```toml
[[sites]]
name        = "cdn-origin"
server_name = ["origin.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/static"
acme_email  = "your@email.com"

[[sites.locations]]
path    = "/"
handler = "static"

[[sites.locations.add_headers]]
name  = "Access-Control-Allow-Origin"
value = "*"

[[sites.locations.add_headers]]
name  = "Timing-Allow-Origin"
value = "*"

[[sites.locations.cache_rules]]
pattern       = ".*"
cache_control = "public, max-age=86400, s-maxage=604800"
```

---

## Directory Listing

> Sweety does not have built-in directory listing. If needed, use an `autoindex` script or the `plugin` extension.

---

## Performance Notes

- Small files (< 512KB) are automatically memory-cached — no disk I/O on hot requests
- Supports `Range` requests (video/audio resume)
- Automatic `gzip`/`brotli` compression negotiation (based on `Accept-Encoding`)
- Large files (> 512KB) use `pread` streaming to avoid loading entirely into memory
