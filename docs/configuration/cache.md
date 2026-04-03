# 缓存

Sweety 提供两种缓存机制：**FastCGI 响应缓存**（PHP）和**反代响应缓存**（HTTP 上游）。

## FastCGI 缓存

等价 Nginx `fastcgi_cache`，缓存 PHP-FPM 的响应。

```toml
[sites.fastcgi.cache]
path               = "/tmp/sweety-fcgi-cache"  # 磁盘缓存目录（不填则纯内存）
max_entries        = 1000                       # 内存缓存最大条数（默认 1000）
ttl                = 60                         # 缓存有效期（秒，默认 60）
cacheable_statuses = [200, 301, 302]            # 可缓存的状态码
cacheable_methods  = ["GET", "HEAD"]            # 可缓存的方法

# 存在这些请求头时跳过缓存（不读也不写）
bypass_headers = []

# 忽略这些响应头对缓存决策的影响
# WordPress 带 Cache-Control: no-store 和 Set-Cookie，需配置此项
ignore_headers = ["Cache-Control", "Set-Cookie"]
```

### WordPress 推荐缓存配置

```toml
[sites.fastcgi.cache]
max_entries    = 2000
ttl            = 300           # 缓存 5 分钟
ignore_headers = ["Cache-Control", "Set-Cookie"]
```

---

## 反代缓存（proxy_cache）

等价 Nginx `proxy_cache`，缓存 HTTP 上游的响应。

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

## 静态文件内存缓存

静态文件自动缓存到内存（LRU），无需配置。缓存策略：

- 文件内容哈希匹配时返回 `304 Not Modified`
- 支持 `ETag` 和 `Last-Modified`
- 压缩版本（gzip/brotli）单独缓存

---

## 缓存字段说明

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `path` | `None` | 磁盘缓存目录，不设则纯内存 |
| `max_entries` | `1000` | 内存中最多缓存多少条响应 |
| `ttl` | `60` | 缓存有效期（秒） |
| `cacheable_statuses` | `[200, 301, 302]` | 哪些状态码可以被缓存 |
| `cacheable_methods` | `["GET", "HEAD"]` | 哪些 HTTP 方法可以被缓存 |
| `bypass_headers` | `[]` | 请求中存在这些头时，跳过缓存 |
| `ignore_headers` | `[]` | 忽略响应中这些头对缓存写入的阻止 |

### `bypass_headers` vs `ignore_headers`

| | `bypass_headers` | `ignore_headers` |
|---|---|---|
| 检查时机 | **请求**到来时 | **响应**返回时 |
| 作用 | 请求头存在 → 不读缓存、不写缓存 | 响应头存在 → 仍然写入缓存 |
| 典型用途 | `Authorization`（登录用户不缓存） | `Cache-Control: no-store`（WordPress 强制缓存） |

---

## 按 Location 设置缓存规则

`[[sites.locations.cache_rules]]` 按文件扩展名设置 `Cache-Control` 响应头：

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

也可以在 location 级别覆盖单个 `cache_control`：

```toml
[[sites.locations]]
path          = "^~ /static/"
handler       = "static"
cache_control = "public, max-age=86400"
```
