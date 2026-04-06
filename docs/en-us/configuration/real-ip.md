# Real IP Configuration

When Sweety is deployed behind multiple proxy layers (CDN / Load Balancer), the client's real IP is replaced by the proxy server's IP. The `real_ip` module extracts the real client IP from request headers, equivalent to Nginx `set_real_ip_from` + `real_ip_header` + `real_ip_recursive`.

## Syntax

```toml
[[sites]]
name        = "my-site"
server_name = ["example.com"]

[sites.real_ip]
set_real_ip_from = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16"]
real_ip_header   = "X-Forwarded-For"
recursive        = true
```

## Options

| Option | Default | Description |
|--------|---------|-------------|
| `set_real_ip_from` | `[]` (empty, disabled) | Trusted proxy IP / CIDR list; extraction only occurs when connection IP matches |
| `real_ip_header` | `"X-Forwarded-For"` | Request header to read real IP from |
| `recursive` | `false` | Recursively skip all trusted IPs from right in X-Forwarded-For |

## How It Works

### X-Forwarded-For Mode

`X-Forwarded-For` header format: `client, proxy1, proxy2`

- **Non-recursive** (`recursive = false`): takes the rightmost IP
- **Recursive** (`recursive = true`): skips all trusted IPs from right to left, takes the first untrusted IP

**Example**: Request passes through two proxy layers to reach Sweety

```
Client 1.2.3.4 → CDN 10.0.0.1 → LB 172.16.1.1 → Sweety
X-Forwarded-For: 1.2.3.4, 10.0.0.1
Connection IP: 172.16.1.1
```

```toml
[sites.real_ip]
set_real_ip_from = ["10.0.0.0/8", "172.16.0.0/12"]
real_ip_header   = "X-Forwarded-For"
recursive        = true
```

- `172.16.1.1` (connection IP) is trusted → allow extraction
- Right to left: `10.0.0.1` is trusted (skip) → `1.2.3.4` is not trusted → **Real IP = 1.2.3.4**

### X-Real-IP Mode

Directly reads the header value as the real IP:

```toml
[sites.real_ip]
set_real_ip_from = ["10.0.0.0/8"]
real_ip_header   = "X-Real-IP"
```

## Security

- **Extraction only occurs when connection IP is in `set_real_ip_from` list**, preventing X-Forwarded-For spoofing
- Module is disabled when trusted list is empty, zero runtime overhead
- CIDR rules are pre-compiled at startup, zero allocation at runtime

## Scope of Effect

When `real_ip` is enabled, the following features automatically use the extracted real client IP:

- **Access log**: `$remote_addr` records real IP
- **IP access control**: `access_rules` matches against real IP
- **Rate limiting**: IP-dimension rate limiting uses real IP
- **auth_request**: Sub-request authentication passes real IP
- **Reverse proxy**: `$remote_addr` in `proxy_set_headers` uses real IP

## Common Configurations

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

### Comparison with PROXY Protocol

| Feature | `real_ip` | `proxy_protocol` |
|---------|-----------|------------------|
| Source | HTTP request header | TCP-level PROXY protocol header |
| Layer | L7 (application) | L4 (transport) |
| Use case | HTTP proxies / CDN | TCP load balancers |
| Security | Relies on trusted list filtering | Connection-level, unforgeable |
| Config location | `[sites.real_ip]` | `proxy_protocol = true` |

Both can be used simultaneously: `proxy_protocol` extracts IP at connection level, `real_ip` further extracts at HTTP level.
