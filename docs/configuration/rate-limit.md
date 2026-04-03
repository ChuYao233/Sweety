# 速率限制

等价 Nginx `limit_req`，基于令牌桶算法，支持多种限流维度。

## 基本配置

```toml
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension = "ip"    # 按客户端 IP 限流
rate      = 100     # 每秒 100 请求
burst     = 200     # 突发容量 200
nodelay   = true    # 超出 burst 立即返回 429，而非排队
```

## 完整字段说明

```toml
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension    = "ip"            # 限流维度（见下表）
rate         = 100             # 稳定速率（每秒请求数）
burst        = 200             # 突发容量（令牌桶上限，默认 = rate）
nodelay      = true            # true = 超出立即 429；false = 超出排队等待
path_pattern = "^/api/"        # 路径正则（dimension = "path" 或 "ip_path" 时使用）
header_name  = "X-User-ID"     # Header 名（dimension = "header" 时使用）
```

## 限流维度

| 值 | 说明 | 示例 |
|----|------|------|
| `ip` | 按客户端 IP | 防 DDoS |
| `path` | 按请求路径 | 限制特定 API |
| `header` | 按指定 Header 值 | 按用户 ID 限流 |
| `user_agent` | 按 User-Agent | 限制爬虫 |
| `ip_path` | IP + 路径组合 | 精细限流 |

## 常用场景

### 全站 IP 限流（防 DDoS）

```toml
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension = "ip"
rate      = 200
burst     = 400
```

### 保护登录接口（防暴力破解）

```toml
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension    = "ip"
rate         = 5
burst        = 10
path_pattern = "^/(wp-login\\.php|login|signin)$"
```

### API 按用户限流

```toml
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension   = "header"
header_name = "X-User-ID"
rate        = 60
burst       = 120
```

### 多规则组合

```toml
[sites.rate_limit]

# 全局 IP 宽限
[[sites.rate_limit.rules]]
dimension = "ip"
rate      = 500
burst     = 1000

# 登录接口严格限制
[[sites.rate_limit.rules]]
dimension    = "ip"
rate         = 5
burst        = 10
path_pattern = "^/wp-login\\.php$"

# API 接口限制
[[sites.rate_limit.rules]]
dimension    = "ip"
rate         = 60
burst        = 120
path_pattern = "^/api/"
```

## 超出限制时的响应

超出速率限制时，Sweety 返回 `429 Too Many Requests`，响应头包含：

```
Retry-After: 1
X-RateLimit-Limit: 100
X-RateLimit-Remaining: 0
```
