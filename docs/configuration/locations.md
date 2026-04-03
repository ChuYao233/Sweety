# 路由规则 [[sites.locations]]

`locations` 定义 URL 路径匹配规则及对应的处理方式，等价 Nginx 的 `location` 块。

## 匹配语法

| 前缀 | 类型 | 示例 |
|------|------|------|
| `= /path` | 精确匹配（优先级最高） | `= /favicon.ico` |
| `^~ /prefix` | 前缀匹配（不继续正则匹配） | `^~ /static/` |
| `~ regex` | 正则匹配（区分大小写） | `~ \.php$` |
| `~* regex` | 正则匹配（不区分大小写） | `~* \.(jpg\|png)$` |
| `/prefix` | 普通前缀匹配 | `/api/` |

匹配优先级：精确 `=` > 前缀 `^~` > 正则 `~`/`~*` > 普通前缀。

## 处理器类型（handler）

| 值 | 说明 |
|----|------|
| `static` | 静态文件服务（默认） |
| `fastcgi` | PHP / FastCGI 转发 |
| `reverse_proxy` | HTTP 反向代理 |
| `grpc` | gRPC 代理 |
| `websocket` | WebSocket 代理 |
| `plugin:<name>` | 自定义插件 |

## 完整配置项

```toml
[[sites.locations]]
path    = "/api/"
handler = "reverse_proxy"
upstream = "backend"         # 引用 [[sites.upstreams]] 的 name

# ─── 根目录覆盖 ──────────────────────────────────────────────
root = "/var/www/other"       # 覆盖站点级 root

# ─── 直接返回 ────────────────────────────────────────────────
return_code = 200             # 返回指定状态码（无 body）
return_url  = "https://new.example.com$request_uri"  # 重定向
return_body = "OK"            # 返回文本内容
return_content_type = "application/json"

# ─── 文件查找 ────────────────────────────────────────────────
try_files = ["$uri", "$uri/", "/index.php?$args"]  # 等价 Nginx try_files

# ─── 响应头控制 ──────────────────────────────────────────────
cache_control = "public, max-age=86400"

[[sites.locations.add_headers]]
name  = "X-Frame-Options"
value = "DENY"

[[sites.locations.proxy_set_headers]]
name  = "X-Real-IP"
value = "$remote_addr"

[[sites.locations.proxy_set_headers]]
name  = "X-Forwarded-Proto"
value = "$scheme"

# ─── 缓存规则（按扩展名） ────────────────────────────────────
[[sites.locations.cache_rules]]
pattern       = "\\.(css|js|woff2?)$"
cache_control = "public, max-age=2592000, immutable"

[[sites.locations.cache_rules]]
pattern       = "\\.(png|jpg|gif|webp|svg|ico)$"
cache_control = "public, max-age=2592000"

# ─── 连接限制 ────────────────────────────────────────────────
limit_conn      = 100         # 并发连接限制（0 = 不限制）
max_connections = 50          # WebSocket 专用最大连接数

# ─── 子请求鉴权（auth_request） ──────────────────────────────
auth_request        = "/auth-check"   # 鉴权子请求路径
auth_failure_status = 401             # 失败返回状态码

[[sites.locations.auth_request_headers]]
name  = "Authorization"
value = "$http_authorization"

# ─── 内容替换（sub_filter） ──────────────────────────────────
[[sites.locations.sub_filter]]
pattern     = "http://old.example.com"
replacement = "https://new.example.com"

# ─── 反向代理 Cookie 处理 ────────────────────────────────────
strip_cookie_secure  = false
proxy_cookie_domain  = "backend.internal example.com"

# ─── 反向代理重定向处理 ──────────────────────────────────────
proxy_redirect_from = "http://backend.internal/"
proxy_redirect_to   = "https://example.com/"

# ─── 缓冲控制 ────────────────────────────────────────────────
proxy_buffering = false   # 关闭缓冲（SSE/流式响应时设为 false）
```

## 支持的变量

在 `value`、`return_url`、`return_body` 等字符串中可使用以下变量：

| 变量 | 说明 |
|------|------|
| `$remote_addr` | 客户端 IP |
| `$host` | 请求 Host 头 |
| `$scheme` | 请求协议（http/https） |
| `$request_uri` | 完整请求路径（含查询字符串） |
| `$uri` | 请求路径（不含查询字符串） |
| `$args` | 查询字符串 |
| `$http_<name>` | 请求头，如 `$http_authorization` |

## 常用示例

### 静态文件加长缓存

```toml
[[sites.locations]]
path    = "~* \\.(js|css|png|jpg|gif|ico|woff2?)$"
handler = "static"

[[sites.locations.cache_rules]]
pattern       = ".*"
cache_control = "public, max-age=2592000, immutable"
```

### PHP 全站转发

```toml
[[sites.locations]]
path      = "~ \\.php$"
handler   = "fastcgi"
try_files = ["$uri", "=404"]
```

### 健康检查端点

```toml
[[sites.locations]]
path        = "= /health"
handler     = "static"
return_code = 200
return_body = "OK"
```

### 强制 CORS 头

```toml
[[sites.locations]]
path    = "/api/"
handler = "reverse_proxy"
upstream = "backend"

[[sites.locations.add_headers]]
name  = "Access-Control-Allow-Origin"
value = "*"

[[sites.locations.add_headers]]
name  = "Access-Control-Allow-Methods"
value = "GET, POST, PUT, DELETE, OPTIONS"
```
