# 快速开始

## 5 分钟启动一个 WordPress 站点

### 1. 创建配置目录

```bash
mkdir -p /etc/sweety
```

### 2. 编写最简配置

`/etc/sweety/sweety.toml`：

```toml
[[sites]]
name        = "wordpress"
server_name = ["php.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/wordpress"
preset      = "wordpress"
php_fastcgi = "/run/php/php8.2-fpm.sock"
acme_email  = "your@email.com"
```

这 8 行配置等价于 50+ 行 Nginx 配置，自动实现：
- HTTP → HTTPS 301 跳转
- Let's Encrypt 证书自动申请与续期
- WordPress 最优 location 路由规则（静态文件直出、PHP 转发、安全过滤）
- HTTP/1.1 + HTTP/2 + HTTP/3 全协议支持

### 3. 校验配置

```bash
sweety validate -c /etc/sweety/sweety.toml
```

### 4. 启动

```bash
# 前台运行（调试时使用）
sweety run -c /etc/sweety/sweety.toml

# 后台运行
sweety start -c /etc/sweety/sweety.toml
```

---

## 5 分钟启动一个静态站点

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

## 5 分钟配置反向代理

```toml
[[sites]]
name        = "api"
server_name = ["api.example.com"]
listen      = [80]
listen_tls  = [443]
acme_email  = "your@email.com"

[[sites.upstreams]]
name  = "backend"
nodes = [
    { addr = "127.0.0.1:3000", weight = 1 }
]

[[sites.locations]]
path    = "/"
handler = "proxy"
upstream = "backend"
```

---

## 多站点示例

```toml
[[sites]]
name        = "site1"
server_name = ["site1.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/site1"
preset      = "static"
acme_email  = "your@email.com"

[[sites]]
name        = "site2"
server_name = ["site2.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/wordpress"
preset      = "wordpress"
php_fastcgi = "/run/php/php8.2-fpm.sock"
acme_email  = "your@email.com"
```

多个站点共享同一端口，Sweety 通过 SNI（TLS）和 `Host` 头（HTTP）自动路由。
