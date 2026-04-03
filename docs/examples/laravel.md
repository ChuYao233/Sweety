# 示例：Laravel

## 最简配置（推荐）

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

> **注意**：`root` 指向 Laravel 项目的 `public` 子目录，不是项目根目录。

---

## 完整配置

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

# 禁止访问 .env 等敏感文件
[[sites.locations]]
path        = "~ /\\.(env|git|htaccess)$"
handler     = "static"
return_code = 403

# 静态资源长缓存（Vite/Mix 编译产物带 hash，可永久缓存）
[[sites.locations]]
path    = "~* \\.(js|css|png|jpg|jpeg|gif|ico|svg|woff|woff2|ttf|eot|webp)$"
handler = "static"

[[sites.locations.cache_rules]]
pattern       = ".*"
cache_control = "public, max-age=2592000, immutable"

# PHP 文件
[[sites.locations]]
path      = "~ \\.php$"
handler   = "fastcgi"
try_files = ["$uri", "=404"]

# Laravel 路由（所有请求转到 index.php）
[[sites.locations]]
path      = "/"
handler   = "static"
try_files = ["$uri", "$uri/", "/index.php?$query_string"]
```

---

## Laravel Octane（Swoole/RoadRunner）

Laravel Octane 启动一个长驻进程，无需 PHP-FPM，直接通过反向代理：

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

## API 站点（纯 JSON API）

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

# 速率限制（API 防滥用）
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension = "ip"
rate      = 60
burst     = 120

# PHP 文件
[[sites.locations]]
path      = "~ \\.php$"
handler   = "fastcgi"
try_files = ["$uri", "=404"]

# Laravel 路由
[[sites.locations]]
path      = "/"
handler   = "static"
try_files = ["$uri", "$uri/", "/index.php?$query_string"]
```

---

## 目录权限

```bash
# 确保 storage 和 bootstrap/cache 可写
chown -R www-data:www-data /var/www/laravel/storage
chown -R www-data:www-data /var/www/laravel/bootstrap/cache
chmod -R 775 /var/www/laravel/storage
chmod -R 775 /var/www/laravel/bootstrap/cache
```
