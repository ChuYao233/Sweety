# HTTP/3 配置与调优

HTTP/3 基于 QUIC（UDP），与 HTTP/2 共享 443 端口。客户端首次通过 HTTP/2 连接，服务器通过 `Alt-Svc` 头广播 HTTP/3 支持，后续请求升级。

## 启用 HTTP/3

HTTP/3 默认**随 HTTPS 自动启用**，无需额外配置：

```toml
[[sites]]
listen_tls = [443]
acme_email = "your@email.com"
# protocols 默认 = ["h3", "h2", "http/1.1"]，h3 自动启用
```

## 禁用 HTTP/3

```toml
[sites.tls]
protocols = ["h2", "http/1.1"]   # 不含 h3
```

## 完整 HTTP/3 调优配置

```toml
[sites.tls.http3]
# ─── 并发控制 ────────────────────────────────────────────────────
max_concurrent_bidi_streams = 200    # 单连接最大并发双向流（默认 200）
max_concurrent_uni_streams  = 100    # 单连接最大并发单向流（默认 100）

# ─── 超时 ────────────────────────────────────────────────────────
idle_timeout_ms          = 30000     # 空闲连接超时（毫秒，默认 30s）
keep_alive_interval_ms   = 10000     # Keep-Alive PING 间隔（毫秒，默认 10s）

# ─── 流量控制窗口 ────────────────────────────────────────────────
receive_window        = 8388608    # 连接级接收窗口（字节，默认 8MB）
stream_receive_window = 2097152    # 流级接收窗口（字节，默认 2MB）
send_window           = 8388608    # 连接级发送窗口（字节，默认 8MB）

# ─── 连接优化 ────────────────────────────────────────────────────
enable_0rtt      = false   # 0-RTT Early Data（默认关闭，存在重放攻击风险）
mtu_discovery    = true    # PMTU 探测（默认开启，优化大包传输）
initial_rtt_ms   = 333     # 初始 RTT 估算（毫秒，quinn 默认值）
max_ack_delay_ms = 25      # 最大 ACK 延迟（毫秒，RFC 9000 默认值）
```

## 全局并发控制

HTTP/3 的全局最大并发 handler 数在 `[global]` 中配置，而非站点级别：

```toml
[global]
# 全局最大并发 H3 handler 数（0 = 自动，按系统可用内存 80% / 2MB 计算）
# 每个 QUIC 连接最多缓冲 send_window 字节，此限制防止 OOM
# 超出时新连接排队等待，不会被拒绝
h3_max_handlers = 0
```

> 压测场景建议手动设置一个较高的值（如 `h3_max_handlers = 5000`），避免自动计算过于保守。

## 调优建议

### 高并发场景

```toml
[sites.tls.http3]
max_concurrent_bidi_streams = 500
receive_window        = 16777216   # 16MB
stream_receive_window = 4194304    # 4MB
send_window           = 16777216   # 16MB
```

### 高延迟网络（跨国/移动网络）

```toml
[sites.tls.http3]
idle_timeout_ms        = 60000   # 延长空闲超时
keep_alive_interval_ms = 15000   # 延长 Keep-Alive 间隔
initial_rtt_ms         = 100     # 手动设置较小初始 RTT（已知延迟时）
```

### 启用 0-RTT（提升首请求速度）

> ⚠️ 0-RTT 存在重放攻击风险，仅对幂等（GET/HEAD）请求安全

```toml
[sites.tls.http3]
enable_0rtt = true
```

## 防火墙配置

HTTP/3 使用 **UDP 443**，必须放行：

```bash
# iptables
iptables -A INPUT -p udp --dport 443 -j ACCEPT

# nftables
nft add rule inet filter input udp dport 443 accept

# firewalld
firewall-cmd --add-port=443/udp --permanent && firewall-cmd --reload

# ufw
ufw allow 443/udp
```

## 验证 HTTP/3

```bash
# 使用 curl（需要 curl >= 7.88 并编译了 HTTP/3 支持）
curl -I --http3 https://your.domain.com

# 响应头应包含
# alt-svc: h3=":443"; ma=86400

# 查看实际使用的协议
curl -I --http3 -w "%{http_version}\n" https://your.domain.com
```

浏览器验证：打开 Chrome DevTools → Network → Protocol 列，应显示 `h3`。
