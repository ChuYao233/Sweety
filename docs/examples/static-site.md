# 示例：静态站点

## 最简配置

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

## SPA（单页应用）

React / Vue / Angular 等 SPA 应用需要将所有路由重定向到 `index.html`：

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

# JS/CSS 构建产物（带 hash，永久缓存）
[[sites.locations]]
path    = "~* \\.(js|css|woff2?|ttf|eot)$"
handler = "static"

[[sites.locations.cache_rules]]
pattern       = ".*"
cache_control = "public, max-age=31536000, immutable"

# 图片资源长缓存
[[sites.locations]]
path    = "~* \\.(png|jpg|jpeg|gif|ico|svg|webp)$"
handler = "static"

[[sites.locations.cache_rules]]
pattern       = ".*"
cache_control = "public, max-age=2592000"

# SPA 路由 fallback（所有未匹配路径返回 index.html）
[[sites.locations]]
path      = "/"
handler   = "static"
try_files = ["$uri", "$uri/", "/index.html"]
```

---

## 静态文档站（Jekyll/Hugo/VitePress）

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

# 安全头
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

# 静态资源缓存
[[sites.locations.cache_rules]]
pattern       = "\\.(js|css|woff2?|png|jpg|svg|ico)$"
cache_control = "public, max-age=86400"
```

---

## 纯静态 + CDN 回源

作为 CDN 回源站，添加跨域头：

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

## 目录浏览（文件列表）

> Sweety 暂不内置目录浏览，如需此功能可配合 `autoindex` 脚本或使用 `plugin` 扩展。

---

## 性能注意

- 小文件（< 512KB）自动内存缓存，热请求无磁盘 I/O
- 支持 `Range` 请求（视频/音频断点续传）
- 自动协商 `gzip`/`brotli` 压缩（基于 `Accept-Encoding`）
- 大文件（> 512KB）使用 `pread` 流式传输，避免一次性加载到内存
