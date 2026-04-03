# 内置预设

`preset` 字段一键应用最优 location 规则，等价手写数十行 `[[sites.locations]]` 配置。

## 用法

```toml
[[sites]]
preset = "wordpress"   # wordpress / laravel / static
```

> **手动优先**：若已存在 `[[sites.locations]]`，`preset` 不生效。

---

## wordpress

WordPress 最优配置，包含：

- 静态资源长缓存（JS/CSS/图片/字体）
- PHP 文件转发到 FastCGI
- WordPress 伪静态（`/index.php?$args`）
- 禁止访问敏感文件（`.php` in `/wp-content/uploads/`、`xmlrpc.php`、`.htaccess`）
- 阻止恶意 User-Agent

等价手动配置：

```toml
# 禁止访问 wp-content/uploads 中的 PHP 文件（防 Webshell）
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

# PHP 文件 → FastCGI
[[sites.locations]]
path     = "~ \\.php$"
handler  = "fastcgi"
try_files = ["$uri", "=404"]

# WordPress 伪静态（所有 URL 路由到 index.php）
[[sites.locations]]
path      = "/"
handler   = "static"
try_files = ["$uri", "$uri/", "/index.php?$args"]
```

### 使用示例

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

Laravel 框架最优配置，包含：

- 静态资源长缓存
- 所有请求路由到 `public/index.php`（Laravel 标准入口）
- 禁止访问 `.env`、`.git` 等敏感目录

### 使用示例

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

纯静态站点最优配置，包含：

- 静态资源长缓存（JS/CSS/图片/字体）
- `try_files $uri $uri/ =404`（标准静态文件行为）
- 压缩自动协商（gzip/brotli）

### 使用示例

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

## 扩展预设（在 preset 基础上追加规则）

`preset` 展开后相当于在 `locations` 列表**前面**插入预设规则。若需追加自定义规则，直接手写：

```toml
[[sites]]
preset      = "wordpress"
php_fastcgi = "/run/php/php8.2-fpm.sock"

# 手动添加限流（在 preset 规则之外生效）
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension = "ip"
rate      = 60
burst      = 100
```

若需要**完全自定义** location，不设 `preset`，手写 `[[sites.locations]]` 即可。
