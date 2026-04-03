# FastCGI / PHP

## 快捷配置（推荐）

```toml
[[sites]]
php_fastcgi = "/run/php/php8.2-fpm.sock"   # Unix Socket
# 或
php_fastcgi = "127.0.0.1:9000"             # TCP
```

等价完整配置：

```toml
[sites.fastcgi]
socket    = "/run/php/php8.2-fpm.sock"
pool_size = 32
```

## 完整 FastCGI 配置

```toml
[sites.fastcgi]
# ─── 连接方式（二选一，socket 优先） ────────────────────────────
socket = "/run/php/php8.2-fpm.sock"   # Unix Socket（推荐，延迟更低）
host   = "127.0.0.1"                  # TCP 主机
port   = 9000                         # TCP 端口

# ─── 连接池 ─────────────────────────────────────────────────────
pool_size = 32           # 连接池大小（默认 32）

# ─── 超时 ───────────────────────────────────────────────────────
connect_timeout = 5      # 连接超时（秒，默认 5）
read_timeout    = 30     # 读取超时（秒，默认 30）

# ─── 响应缓存（等价 Nginx fastcgi_cache） ────────────────────────
[sites.fastcgi.cache]
path            = "/tmp/sweety-fcgi-cache"  # 磁盘缓存目录（不设则纯内存）
max_entries     = 1000                      # 内存缓存最大条数
ttl             = 60                        # 缓存有效期（秒）
cacheable_statuses = [200, 301, 302]
cacheable_methods  = ["GET", "HEAD"]

# 跳过缓存的请求头（存在此头时不读缓存）
bypass_headers = []

# 忽略响应头对缓存决策的影响
# WordPress 需要配置此项，否则 Cache-Control: no-store 会阻止缓存
ignore_headers = ["Cache-Control", "Set-Cookie"]
```

## FastCGI Location 配置

在 `[[sites.locations]]` 中将 `handler` 设为 `fastcgi`：

```toml
[[sites.locations]]
path    = "~ \\.php$"
handler = "fastcgi"

# 可选：覆盖根目录
# root = "/var/www/other"
```

## 常见 PHP-FPM 配置

### Ubuntu/Debian

```bash
# PHP 8.2 的 socket 路径
/run/php/php8.2-fpm.sock
```

### CentOS/RHEL

```bash
/run/php-fpm/www.sock
```

### 宝塔面板（BT Panel）

```bash
/tmp/php-cgi-82.sock   # PHP 8.2
/tmp/php-cgi-80.sock   # PHP 8.0
/tmp/php-cgi-74.sock   # PHP 7.4
```

### 多 PHP 版本共存

```toml
[[sites]]
name        = "php82-site"
server_name = ["php82.example.com"]
php_fastcgi = "/run/php/php8.2-fpm.sock"

[[sites]]
name        = "php74-site"
server_name = ["php74.example.com"]
php_fastcgi = "/run/php/php7.4-fpm.sock"
```

## FastCGI 缓存调优

### WordPress 缓存配置

WordPress 的每个响应通常带有 `Cache-Control: no-store` 和 `Set-Cookie` 头，会阻止默认缓存行为。使用 `ignore_headers` 强制缓存：

```toml
[sites.fastcgi.cache]
max_entries    = 2000
ttl            = 300           # 缓存 5 分钟
ignore_headers = ["Cache-Control", "Set-Cookie"]  # 忽略阻止缓存的响应头
bypass_headers = []            # 不因请求头跳过缓存
```

> 注意：开启强制缓存后，已登录用户可能看到其他人的页面。建议在 `[[sites.locations]]` 中为登录用户单独设置不缓存的 location。

### 性能建议

- **Unix Socket** 比 TCP 延迟低约 10-20%，同机部署时优先使用
- `pool_size` 设置为 PHP-FPM `pm.max_children` 的 60-80%，避免连接排队
- `ttl` 根据内容更新频率设置，新闻类站点可设 30-60 秒，静态内容可设 3600 秒
