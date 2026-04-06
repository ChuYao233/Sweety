# Real IP 配置

当 Sweety 部署在多层代理（CDN / 负载均衡器）后面时，客户端的真实 IP 被代理层替换为代理服务器的 IP。`real_ip` 模块从请求头中提取真实客户端 IP，等价 Nginx `set_real_ip_from` + `real_ip_header` + `real_ip_recursive`。

## 配置语法

```toml
[[sites]]
name        = "my-site"
server_name = ["example.com"]

[sites.real_ip]
set_real_ip_from = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"]
real_ip_header   = "X-Forwarded-For"
recursive        = true
```

## 配置项

| 配置项 | 默认值 | 说明 |
|--------|--------|------|
| `set_real_ip_from` | `[]`（空，不启用） | 受信代理 IP / CIDR 列表，仅当连接 IP 匹配时才从 header 提取 |
| `real_ip_header` | `"X-Forwarded-For"` | 从哪个请求头读取真实 IP |
| `recursive` | `false` | 是否递归查找（从 X-Forwarded-For 右侧开始跳过所有受信 IP） |

## 工作原理

### X-Forwarded-For 模式

`X-Forwarded-For` 头格式：`client, proxy1, proxy2`

- **非递归**（`recursive = false`）：取最右侧 IP
- **递归**（`recursive = true`）：从右向左跳过所有在 `set_real_ip_from` 中的受信 IP，取第一个非受信 IP

**示例**：请求经过两层代理到达 Sweety

```
客户端 1.2.3.4 → CDN 10.0.0.1 → LB 172.16.1.1 → Sweety
X-Forwarded-For: 1.2.3.4, 10.0.0.1
连接 IP: 172.16.1.1
```

```toml
[sites.real_ip]
set_real_ip_from = ["10.0.0.0/8", "172.16.0.0/12"]
real_ip_header   = "X-Forwarded-For"
recursive        = true
```

- `172.16.1.1`（连接 IP）在受信列表 → 允许提取
- 从右向左：`10.0.0.1` 在受信列表（跳过）→ `1.2.3.4` 不在受信列表 → **真实 IP = 1.2.3.4**

### X-Real-IP 模式

直接读取头值作为真实 IP：

```toml
[sites.real_ip]
set_real_ip_from = ["10.0.0.0/8"]
real_ip_header   = "X-Real-IP"
```

## 安全说明

- **仅当连接 IP 在 `set_real_ip_from` 列表中时才替换**，防止客户端伪造 X-Forwarded-For
- 受信列表为空时模块不启用，零运行时开销
- CIDR 规则在启动时预编译，运行时零分配

## 影响范围

启用 `real_ip` 后，以下功能自动使用提取后的真实客户端 IP：

- **访问日志**：`$remote_addr` 记录真实 IP
- **IP 访问控制**：`access_rules` 基于真实 IP 判断
- **限流**：IP 维度限流基于真实 IP
- **auth_request**：子请求鉴权传递真实 IP
- **反向代理**：`proxy_set_headers` 中 `$remote_addr` 使用真实 IP

## 常用配置

### Cloudflare CDN

```toml
[sites.real_ip]
set_real_ip_from = [
    "173.245.48.0/20",
    "103.21.244.0/22",
    "103.22.200.0/22",
    "103.31.4.0/22",
    "141.101.64.0/18",
    "108.162.192.0/18",
    "190.93.240.0/20",
    "188.114.96.0/20",
    "197.234.240.0/22",
    "198.41.128.0/17",
    "162.158.0.0/15",
    "104.16.0.0/13",
    "104.24.0.0/14",
    "172.64.0.0/13",
    "131.0.72.0/22",
]
real_ip_header = "CF-Connecting-IP"
```

### AWS ALB / ELB

```toml
[sites.real_ip]
set_real_ip_from = ["10.0.0.0/8", "172.16.0.0/12"]
real_ip_header   = "X-Forwarded-For"
recursive        = true
```

### 与 PROXY Protocol 的区别

| 特性 | `real_ip` | `proxy_protocol` |
|------|-----------|------------------|
| 信息来源 | HTTP 请求头 | TCP 连接级别 PROXY protocol 头 |
| 协议层 | L7（应用层） | L4（传输层） |
| 适用场景 | HTTP 代理 / CDN | TCP 负载均衡器 |
| 安全性 | 依赖受信列表过滤 | 连接级别，不可伪造 |
| 配置位置 | `[sites.real_ip]` | `proxy_protocol = true` |

两者可同时使用：`proxy_protocol` 在连接层提取 IP，`real_ip` 在 HTTP 层进一步提取。
