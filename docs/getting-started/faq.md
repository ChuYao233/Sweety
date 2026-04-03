# 常见问题

## 启动问题

### 端口被占用

```
Error: Address already in use (os error 98)
```

查找占用端口的进程：

```bash
ss -tlnp | grep :443
# 或
fuser 443/tcp
```

### 证书找不到

```
TLS error: no certificate found for example.com
```

- 检查 `root` 路径是否正确
- 若使用 ACME，确认 `acme_email` 已填写
- 若使用手动证书，确认 `cert` / `key` 路径存在且可读
- 运行 `sweety validate` 查看详细错误

### 权限不足（绑定 80/443）

```bash
# 方法一：setcap（推荐，不需要 root 运行）
sudo setcap 'cap_net_bind_service=+ep' /usr/local/bin/sweety

# 方法二：使用 systemd（推荐），在 [Service] 中添加
AmbientCapabilities=CAP_NET_BIND_SERVICE
```

---

## ACME / 证书问题

### 证书申请失败

1. 确认域名 DNS 已解析到服务器 IP
2. 确认 80 端口可访问（HTTP-01 验证需要）
3. Let's Encrypt 有速率限制（每域名每周 5 次），测试时使用 Staging 环境：

```toml
[sites.tls]
acme         = true
acme_email   = "your@email.com"
acme_provider = "https://acme-staging-v02.api.letsencrypt.org/directory"
```

### HTTPS 跳转后浏览器报"重定向次数过多"

确认 `force_https = true` 只在 HTTP 站点配置，TLS 站点不要再配置：

```toml
[[sites]]
listen     = [80]
listen_tls = [443]
force_https = true   # 只对 HTTP 80 生效，HTTPS 请求不会再跳
```

---

## FastCGI / PHP 问题

### PHP 返回 502

1. 检查 PHP-FPM 是否在运行：`systemctl status php8.2-fpm`
2. 检查 socket 路径：`ls -la /run/php/php8.2-fpm.sock`
3. 确认 Sweety 运行用户有权限访问 socket

### PHP 上传文件失败

```toml
[global]
client_max_body_size = 100   # MB，默认 50MB
```

同时确认 php.ini 中 `upload_max_filesize` 和 `post_max_size` 也足够大。

---

## HTTP/3 问题

### 浏览器不使用 HTTP/3

1. 确认防火墙已放行 UDP 443 端口
2. 确认 TLS 证书有效（HTTP/3 不接受自签名证书）
3. 首次访问 HTTP/2，浏览器会通过 `Alt-Svc` 头发现 HTTP/3，第二次请求才升级

### 验证 HTTP/3 是否工作

```bash
curl -I --http3 https://your.domain.com
# 响应头中应有 alt-svc: h3=":443"
```

---

## 热重载问题

### `sweety reload` 报错

确认 `global.admin_listen` 已配置：

```toml
[global]
admin_listen = "127.0.0.1:9099"
```

重载命令会向 Admin API 发送信号，不配置则无法热重载。

---

## 性能问题

### 高并发下 503

调整：

```toml
[global]
worker_threads     = 0      # 0 = 自动检测 CPU 核心数
worker_connections = 51200
max_connections    = 50000
```

系统层面：

```bash
# 增大文件描述符限制
ulimit -n 65535
# 或在 /etc/security/limits.conf 中配置
```
