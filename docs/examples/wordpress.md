# 示例：WordPress

## 最简配置（推荐）

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

8 行配置自动包含：ACME 自动 HTTPS、HTTP→HTTPS 跳转、WordPress 伪静态、PHP 转发、静态资源缓存、安全过滤规则。

---

## 带缓存的完整配置

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

# FastCGI 响应缓存
[sites.fastcgi.cache]
max_entries    = 2000
ttl            = 300
ignore_headers = ["Cache-Control", "Set-Cookie"]

# HSTS
[sites.hsts]
max_age            = 31536000
include_subdomains = true

# 限流：防暴力登录
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension    = "ip"
rate         = 30
burst        = 60
path_pattern = "^/wp-login\\.php$"
```

---

## 多站点 WordPress

```toml
# 站点 1
[[sites]]
name        = "blog1"
server_name = ["blog1.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/blog1"
preset      = "wordpress"
php_fastcgi = "/run/php/php8.2-fpm.sock"
acme_email  = "your@email.com"

# 站点 2（不同 PHP 版本）
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

## 手动完整配置（不使用 preset）

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

# 禁止 wp-content/uploads 中执行 PHP（防 Webshell）
[[sites.locations]]
path        = "~ /wp-content/uploads/.*\\.php$"
handler     = "static"
return_code = 403

# 禁止访问敏感文件
[[sites.locations]]
path        = "~ ^/(xmlrpc\\.php|\\.htaccess|\\.env)$"
handler     = "static"
return_code = 403

# 静态资源长缓存
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

# WordPress 伪静态
[[sites.locations]]
path      = "/"
handler   = "static"
try_files = ["$uri", "$uri/", "/index.php?$args"]
```

---

## 性能调优建议

### PHP-FPM 配置（`/etc/php/8.2/fpm/pool.d/www.conf`）

```ini
pm = dynamic
pm.max_children      = 50
pm.start_servers     = 10
pm.min_spare_servers = 5
pm.max_spare_servers = 20
pm.max_requests      = 500
```

`pool_size` 建议设为 `pm.max_children` 的 70%（即 35）：

```toml
[sites.fastcgi]
socket    = "/run/php/php8.2-fpm.sock"
pool_size = 35
```

### OPcache（`/etc/php/8.2/fpm/conf.d/10-opcache.ini`）

```ini
opcache.enable           = 1
opcache.memory_consumption = 256
opcache.max_accelerated_files = 20000
opcache.revalidate_freq  = 0
opcache.validate_timestamps = 0
```
