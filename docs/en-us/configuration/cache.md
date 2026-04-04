# Cache

Sweety provides two caching mechanisms: **FastCGI response cache** (PHP) and **proxy response cache** (HTTP upstream).

## FastCGI Cache

Equivalent to Nginx `fastcgi_cache`, caches PHP-FPM responses.

```toml
[sites.fastcgi.cache]
path               = "/tmp/sweety-fcgi-cache"  # Disk cache directory (omit for memory-only)
max_entries        = 1000                       # Max memory cache entries (default 1000)
ttl                = 60                         # Cache TTL (seconds, default 60)
cacheable_statuses = [200, 301, 302]            # Cacheable status codes
cacheable_methods  = ["GET", "HEAD"]            # Cacheable methods

# Skip cache when these request headers are present (no read, no write)
bypass_headers = []

# Ignore these response headers' effect on cache decisions
# WordPress sends Cache-Control: no-store and Set-Cookie, configure this to cache anyway
ignore_headers = ["Cache-Control", "Set-Cookie"]
```

### WordPress Recommended Cache Config

```toml
[sites.fastcgi.cache]
max_entries    = 2000
ttl            = 300           # Cache for 5 minutes
ignore_headers = ["Cache-Control", "Set-Cookie"]
```

---

## Proxy Cache (proxy_cache)

Equivalent to Nginx `proxy_cache`, caches HTTP upstream responses.

```toml
[sites.proxy_cache]
path               = "/tmp/sweety-proxy-cache"
max_entries        = 1000
ttl                = 60
cacheable_statuses = [200, 301, 302]
cacheable_methods  = ["GET", "HEAD"]
bypass_headers     = ["Authorization", "Cookie"]
ignore_headers     = []
```

---

## Static File Memory Cache

Static files are automatically cached in memory (LRU) — no configuration needed. Cache behavior:

- Returns `304 Not Modified` when file content hash matches
- Supports `ETag` and `Last-Modified`
- Compressed versions (gzip/brotli) are cached separately

---

## Cache Field Reference

| Field | Default | Description |
|-------|---------|-------------|
| `path` | `None` | Disk cache directory, omit for memory-only |
| `max_entries` | `1000` | Max cached responses in memory |
| `ttl` | `60` | Cache TTL (seconds) |
| `cacheable_statuses` | `[200, 301, 302]` | Which status codes can be cached |
| `cacheable_methods` | `["GET", "HEAD"]` | Which HTTP methods can be cached |
| `bypass_headers` | `[]` | Skip cache when these request headers are present |
| `ignore_headers` | `[]` | Ignore these response headers that would block caching |

### `bypass_headers` vs `ignore_headers`

| | `bypass_headers` | `ignore_headers` |
|---|---|---|
| Check timing | On **request** arrival | On **response** return |
| Effect | Request header present → skip cache read & write | Response header present → still write to cache |
| Typical use | `Authorization` (don't cache logged-in users) | `Cache-Control: no-store` (force-cache WordPress) |

---

## Per-Location Cache Rules

`[[sites.locations.cache_rules]]` sets `Cache-Control` response headers by file extension:

```toml
[[sites.locations]]
path    = "/"
handler = "static"

[[sites.locations.cache_rules]]
pattern       = "\\.(js|css|woff2?)$"
cache_control = "public, max-age=2592000, immutable"

[[sites.locations.cache_rules]]
pattern       = "\\.(png|jpg|gif|webp|ico)$"
cache_control = "public, max-age=2592000"

[[sites.locations.cache_rules]]
pattern       = "\\.html$"
cache_control = "public, max-age=3600"
```

You can also override `cache_control` at the location level:

```toml
[[sites.locations]]
path          = "^~ /static/"
handler       = "static"
cache_control = "public, max-age=86400"
```
