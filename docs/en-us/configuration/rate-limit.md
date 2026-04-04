# Rate Limiting

Equivalent to Nginx `limit_req`, based on token bucket algorithm, supporting multiple rate limiting dimensions.

## Basic Configuration

```toml
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension = "ip"    # Rate limit by client IP
rate      = 100     # 100 requests per second
burst     = 200     # Burst capacity 200
nodelay   = true    # Exceed burst → immediately return 429, instead of queuing
```

## Full Field Reference

```toml
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension    = "ip"            # Rate limit dimension (see table below)
rate         = 100             # Steady rate (requests per second)
burst        = 200             # Burst capacity (token bucket max, default = rate)
nodelay      = true            # true = 429 immediately; false = queue and wait
path_pattern = "^/api/"        # Path regex (used when dimension = "path" or "ip_path")
header_name  = "X-User-ID"     # Header name (used when dimension = "header")
```

## Rate Limit Dimensions

| Value | Description | Example Use |
|-------|-------------|-------------|
| `ip` | By client IP | DDoS protection |
| `path` | By request path | Limit specific APIs |
| `header` | By specified header value | Rate limit by user ID |
| `user_agent` | By User-Agent | Limit crawlers |
| `ip_path` | IP + path combination | Fine-grained limiting |

## Common Scenarios

### Site-wide IP Limiting (DDoS Protection)

```toml
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension = "ip"
rate      = 200
burst     = 400
```

### Protect Login Endpoint (Brute Force Prevention)

```toml
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension    = "ip"
rate         = 5
burst        = 10
path_pattern = "^/(wp-login\\.php|login|signin)$"
```

### API Per-User Rate Limiting

```toml
[sites.rate_limit]
[[sites.rate_limit.rules]]
dimension   = "header"
header_name = "X-User-ID"
rate        = 60
burst       = 120
```

### Multiple Rules Combined

```toml
[sites.rate_limit]

# Global IP allowance
[[sites.rate_limit.rules]]
dimension = "ip"
rate      = 500
burst     = 1000

# Strict login endpoint limit
[[sites.rate_limit.rules]]
dimension    = "ip"
rate         = 5
burst        = 10
path_pattern = "^/wp-login\\.php$"

# API endpoint limit
[[sites.rate_limit.rules]]
dimension    = "ip"
rate         = 60
burst        = 120
path_pattern = "^/api/"
```

## Response When Rate Exceeded

When the rate limit is exceeded, Sweety returns `429 Too Many Requests` with headers:

```
Retry-After: 1
X-RateLimit-Limit: 100
X-RateLimit-Remaining: 0
```
