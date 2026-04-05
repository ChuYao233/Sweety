# TLS / HTTPS / ACME

## 最简 HTTPS（ACME 自动证书）

```toml
[[sites]]
name        = "my-site"
server_name = ["example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/html"
acme_email  = "your@email.com"   # 一行开启 ACME 自动 HTTPS
```

等价完整写法：

```toml
[sites.tls]
acme       = true
acme_email = "your@email.com"
```

## 完整 TLS 配置

```toml
[sites.tls]
# ─── 证书来源（三选一） ──────────────────────────────────────────
# 方式 1：ACME 自动证书
acme             = true
acme_email       = "your@email.com"
acme_provider    = "letsencrypt"   # letsencrypt / zerossl / litessl / 自定义 URL
acme_challenge   = "http01"        # http01 / dns01
acme_renew_days_before = 30        # 到期前 N 天自动续期

# 方式 2：手动单证书
cert = "/etc/ssl/example.com.crt"
key  = "/etc/ssl/example.com.key"

# 方式 3：多证书（SNI 路由，同端口不同域名不同证书）
[[sites.tls.certs]]
cert = "/etc/ssl/example.com.crt"
key  = "/etc/ssl/example.com.key"

[[sites.tls.certs]]
cert = "/etc/ssl/example.org.crt"
key  = "/etc/ssl/example.org.key"

# ─── TLS 版本控制 ─────────────────────────────────────────────
min_version = "tls1.2"   # tls1.2 / tls1.3（默认 tls1.2）
max_version = "tls1.3"   # 默认 tls1.3

# ─── 协议列表（ALPN，影响 HTTP/2 和 HTTP/3 协商）──────────────
protocols = ["h3", "h2", "http/1.1"]   # 默认全开，顺序即优先级

# ─── HTTP/3 QUIC 调优 ─────────────────────────────────────────
[sites.tls.http3]
max_concurrent_bidi_streams = 200
max_concurrent_uni_streams  = 100
idle_timeout_ms              = 30000
keep_alive_interval_ms       = 10000
receive_window               = 8388608   # 8MB
stream_receive_window        = 2097152   # 2MB
send_window                  = 8388608   # 8MB
enable_0rtt                  = false
mtu_discovery                = true
initial_rtt_ms               = 333
max_ack_delay_ms             = 25
```

## ACME 多域名 SAN 证书

当一个站点配置了多个 `server_name` 时，ACME 自动签发一张 **SAN 证书**覆盖所有域名，无需额外配置：

```toml
[[sites]]
name        = "my-site"
server_name = ["example.com", "www.example.com", "api.example.com"]
listen      = [80]
listen_tls  = [443]

[sites.tls]
acme       = true
acme_email = "admin@example.com"
# → 自动签发一张包含 3 个域名的 SAN 证书
```

- 证书文件以第一个非通配符域名命名（如 `example.com.crt`）
- 每 12 小时自动检查续期（到期前 30 天续期）
- 续期失败不影响当前证书，仅记录日志

## ACME 即时续期

通过 Admin API 可立即触发证书续期（不等待自动检查周期）：

```bash
# 续期所有 ACME 站点
curl -X POST http://127.0.0.1:9099/api/certs/acme/renew \
  -H "Authorization: Bearer $TOKEN"

# 仅续期指定站点
curl -X POST "http://127.0.0.1:9099/api/certs/acme/renew?site=my-site" \
  -H "Authorization: Bearer $TOKEN"
```

返回 202 Accepted，续期在后台异步执行。详见 [管理 API](./admin-api.md)。

## ACME DNS-01 验证（通配符证书）

DNS-01 验证可申请 `*.example.com` 通配符证书：

```toml
[sites.tls]
acme           = true
acme_email     = "your@email.com"
acme_challenge = "dns01"

# Cloudflare DNS
[sites.tls.dns_provider]
type      = "cloudflare"
api_token = "your-cloudflare-api-token"
zone_id   = "optional-zone-id"   # 不填则自动查找

# 阿里云 DNS
# [sites.tls.dns_provider]
# type              = "aliyun"
# access_key_id     = "your-key-id"
# access_key_secret = "your-key-secret"

# 自定义 Shell 脚本
# [sites.tls.dns_provider]
# type       = "shell"
# set_script = "/etc/sweety/dns-set.sh"
# del_script = "/etc/sweety/dns-del.sh"  # 可选
```

## 协议控制

`protocols` 字段控制站点支持哪些 HTTP 版本，作用于 ALPN 协商：

```toml
# 只支持 HTTP/1.1（禁用 H2/H3）
protocols = ["http/1.1"]

# 只支持 HTTP/2
protocols = ["h2"]

# 只支持 HTTP/3（不推荐，浏览器首次无法发现）
protocols = ["h3"]

# 默认（全部支持）
protocols = ["h3", "h2", "http/1.1"]
```

多站点共享同一 TLS 端口时，ALPN 协议列表取所有站点的**并集**，即只要有一个站点支持 h3，该端口就启用 UDP 监听。

## HTTP/3 防火墙注意事项

HTTP/3 使用 UDP 443，确认防火墙已放行：

```bash
# iptables
iptables -A INPUT -p udp --dport 443 -j ACCEPT

# firewalld
firewall-cmd --add-port=443/udp --permanent
firewall-cmd --reload
```
