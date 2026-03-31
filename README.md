# Sweety

高性能、单文件部署的多站点 Web 服务器，Rust 编写，基于 [xitca-web](https://github.com/HFQR/xitca-web)。

---

## 特性

### 协议 & 传输
- **HTTP/1.1 + HTTP/2 + HTTP/3（QUIC）**，同一进程同时监听
- **TLS**：Rustls 纯 Rust 实现，无 OpenSSL 依赖；支持 ECDSA / RSA / Ed25519 多证书，SNI 自动选最优
- **ACME 自动证书**：HTTP-01 验证，支持 Let's Encrypt / ZeroSSL / Buypass，到期前自动续期
- **WebSocket** 代理（HTTP → WS/WSS 升级）

### 请求处理
- **静态文件**：内存缓存（≤ 256 KB）+ 磁盘流式传输，notify 监听自动失效，支持 Range、ETag/Last-Modified、`try_files`
- **PHP/FastCGI**：Unix Socket / TCP 连接池，死连接自动剔除，流式响应，`fastcgi_cache`
- **反向代理**：轮询 / 加权 / 最少连接 / IP 哈希，TCP 连接池复用，主动健康检查，`proxy_cache`
- **gRPC 代理**：`handler = "grpc"`，自动处理 `Content-Type: application/grpc` 和 `grpc-status` Trailer
- **`auth_request`** 子请求鉴权（等价 Nginx `auth_request`）

### 路由
- 虚拟主机（精确 / 通配符 `*.example.com` / `fallback` 兜底）
- Location 四级优先级：`= 精确` > `^~ 前缀优先` > `~ 正则` > `普通前缀`
- Rewrite 规则：正则捕获组替换，`last / break / redirect / permanent` 标志，`!-f / !-d` 条件

### 响应 & 内容
- **Brotli + gzip** 双压缩（优先 `br`，压缩率高 15-30%），全局/站点级覆盖
- **`return_body`**：直接返回文本内容体，可指定 Content-Type（等价 Caddy `respond`）
- **`sub_filter`**：响应体字符串/正则替换（等价 Nginx `sub_filter`）
- `add_headers` / `proxy_set_headers` / `cache_rules` / `strip_cookie_secure` / `proxy_redirect`

### 限流 & 安全
- **五维度令牌桶限流**：IP / 路径 / IP+路径 / Header / User-Agent，`nodelay` 模式
- **`limit_conn`**：per-location 并发连接限制
- 敏感路径自动拦截（`.git` / `.env` / `composer.json` 等）
- HSTS 注入，`force_https` 自动 301 跳转，HTTPS 跨站防护（421）

### 日志 & 监控
- 访问日志异步写文件（独立系统线程，不占 tokio worker）
- 日志格式：`combined`（Apache 格式）/ `json`（结构化）/ **自定义 `log_format` 变量模板**
- Prometheus 指标（`/metrics`）+ 管理 API（HTTP + WebSocket）

### 运维
- **配置热重载**：文件变更后 diff 更新，不断开现有连接，等价 `nginx -s reload`
- **`-t` 配置测试**：启动前验证配置文件和 TLS 证书，等价 `nginx -t`
- 支持 TOML / JSON / YAML 配置文件（扩展名自动识别）

---

## 快速开始

### 编译

```bash
cargo build --release
# 二进制：target/release/sweety
```

### 运行

```bash
# 启动
./sweety --config config/sweety.toml

# 测试配置（不启动服务器）
./sweety --config config/sweety.toml --test
```

### 最简静态站点

```toml
[[sites]]
name        = "my-site"
server_name = ["example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/example"
force_https = true

[sites.tls]
acme       = true
acme_email = "admin@example.com"

[[sites.locations]]
path    = "/"
handler = "static"
```

### PHP + WordPress

```toml
[[sites]]
name        = "wordpress"
server_name = ["blog.example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/wordpress"
index       = ["index.php", "index.html"]
force_https = true

[sites.tls]
acme = true
acme_email = "admin@example.com"

[sites.fastcgi]
socket = "/var/run/php/php8.2-fpm.sock"

[[sites.rewrites]]
pattern   = "^/(.+)$"
target    = "/index.php?$1"
flag      = "last"
condition = "!-f"

[[sites.locations]]
path    = "~ \\.php$"
handler = "fastcgi"

[[sites.locations]]
path      = "/"
handler   = "fastcgi"
try_files = ["$uri", "$uri/", "/index.php"]
```

### 反向代理

```toml
[[sites]]
name        = "api"
server_name = ["api.example.com"]
listen      = [80]
listen_tls  = [443]
force_https = true

[sites.tls]
acme = true
acme_email = "admin@example.com"

[[sites.upstreams]]
name     = "backend"
strategy = "least_conn"  # round_robin / weighted / least_conn / ip_hash

[[sites.upstreams.nodes]]
addr = "127.0.0.1:8001"

[[sites.upstreams.nodes]]
addr   = "127.0.0.1:8002"
weight = 2

[[sites.locations]]
path     = "/"
handler  = "reverse_proxy"
upstream = "backend"
```

完整配置参考 [`config/sweety.example.toml`](config/sweety.example.toml)。

---

## 项目结构

```
sweety/
├─ src/
│  ├─ main.rs              # CLI 入口（--config / --test）
│  ├─ server/
│  │  ├─ http.rs           # xitca-web 应用构建、多站点分发、AppState
│  │  ├─ tls.rs            # Rustls SNI Resolver、ACME 自动续期
│  │  └─ quic.rs           # HTTP/3 Quinn 集成
│  ├─ dispatcher/
│  │  ├─ vhost.rs          # 虚拟主机注册表（ArcSwap 无锁热更新）
│  │  ├─ location.rs       # Location 优先级匹配、per-location conn_count
│  │  └─ rewrite.rs        # Rewrite 规则引擎
│  ├─ handler/
│  │  ├─ static_file.rs    # 静态文件（内存缓存 + notify 失效 + Range + br/gz）
│  │  ├─ fastcgi.rs        # FastCGI 协议实现 + 连接池
│  │  ├─ reverse_proxy/    # 反向代理（LB + 健康检查 + 连接池 + WS）
│  │  ├─ grpc.rs           # gRPC 代理
│  │  ├─ auth_request.rs   # 子请求鉴权
│  │  ├─ websocket.rs      # WebSocket 升级代理
│  │  └─ error_page.rs     # 错误页（预构建 Bytes 缓存）
│  ├─ middleware/
│  │  ├─ access_log.rs     # 异步访问日志（combined/json/自定义模板）
│  │  ├─ rate_limit.rs     # 五维度令牌桶限流
│  │  ├─ security.rs       # 敏感路径拦截
│  │  ├─ proxy_cache.rs    # 反代/FastCGI 响应缓存
│  │  ├─ cache.rs          # ETag/Last-Modified 协商缓存
│  │  └─ metrics.rs        # Prometheus 原子计数器
│  ├─ config/
│  │  ├─ model.rs          # 所有配置结构体
│  │  ├─ loader.rs         # TOML/JSON/YAML 加载
│  │  └─ hot_reload.rs     # notify 配置热重载
│  ├─ monitor/             # 慢请求统计、Prometheus 导出
│  └─ admin_api/           # 管理 API（HTTP REST + WebSocket 推送）
└─ config/
   ├─ sweety.example.toml  # 完整配置示例（含所有配置项注释）
   └─ docs/architecture.md # 架构文档
```

---

## 配置速查

| 配置块 | 常用字段 |
|---|---|
| `[global]` | `worker_threads` `worker_connections` `max_connections` `keepalive_timeout` `client_max_body_size` `gzip` `gzip_comp_level` `admin_listen` `admin_token` `log_level` |
| `[[sites]]` | `name` `server_name` `listen` `listen_tls` `root` `index` `access_log` `access_log_format` `force_https` `fallback` `gzip` `websocket` `error_pages` |
| `[sites.tls]` | `acme` `acme_email` `acme_provider` `acme_renew_days_before` `cert` `key` `certs[]` `min_version` `max_version` |
| `[sites.hsts]` | `max_age` `include_sub_domains` `preload` |
| `[sites.fastcgi]` | `socket` `host` `port` `pool_size` `connect_timeout` `read_timeout` `cache{}` |
| `[sites.proxy_cache]` | `path` `max_entries` `ttl` `cacheable_statuses` `cacheable_methods` `bypass_headers` |
| `[[sites.upstreams]]` | `name` `strategy` `keepalive` `keepalive_requests` `keepalive_time` `health_check{}` `nodes[]{addr,weight,tls,tls_sni,upstream_host}` |
| `[[sites.rewrites]]` | `pattern` `target` `flag`（last/break/redirect/permanent）`condition`（!-f/!-d）|
| `[sites.rate_limit]` | `rules[]{dimension,rate,burst,nodelay,path_pattern,header_name}` |
| `[[sites.locations]]` | `path` `handler` `upstream` `root` `try_files` `return_code` `return_url` `return_body` `return_content_type` `limit_conn` `proxy_buffering` `cache_control` `auth_request` `auth_failure_status` `proxy_set_headers[]` `add_headers[]` `sub_filter[]` `cache_rules[]` |

### 访问日志格式（`access_log_format`）

| 值 | 说明 |
|---|---|
| `"combined"` | Apache Combined 格式（默认） |
| `"json"` | 结构化 JSON，适合 ELK/Loki |
| 自定义模板 | 支持变量 `$remote_addr` `$method` `$uri` `$status` `$bytes_sent` `$http_referer` `$http_user_agent` `$duration_ms` `$time_local` `$site` |

---

## 与 Nginx / Caddy 对比

| 功能 | Sweety | Nginx | Caddy |
|---|---|---|---|
| HTTP/3 内置 | ✅ | ❌ 需重新编译 | ✅ |
| ACME 自动证书 | ✅ HTTP-01 | ❌ 需 certbot | ✅ 零配置 |
| Brotli 压缩内置 | ✅ | ❌ 第三方模块 | ✅ |
| FastCGI 连接池 | ✅ | ✅ | ✅ |
| auth_request | ✅ | ✅ | ❌ 需插件 |
| gRPC 代理 | ✅ | ✅ | ✅ |
| WebSocket 代理 | ✅ | ✅ | ✅ |
| respond 直接返回内容 | ✅ | ❌ 仅状态码 | ✅ |
| 配置热重载不断连 | ✅ | ✅ reload | ✅ |
| 单文件无依赖 | ✅ | ❌ | ✅ |
| 内存安全 | ✅ Rust | ❌ C | ✅ Go |
| GC 暂停 | ✅ 无 | ✅ 无 | ⚠️ Go GC |
| Windows 多线程 | ✅ IOCP | ❌ 单线程 select | ⚠️ |
| ACME DNS-01 / 通配符证书 | ❌ 规划中 | ❌ 需 certbot | ✅ |
| `if` 条件块 / `map` 变量 | ❌ | ✅ | ✅ |
| TCP/UDP 四层代理 | ❌ | ✅ stream | ✅ layer4 |
| Lua/插件扩展 | ❌ | ✅ OpenResty | ✅ 插件生态 |

---

## 已知问题

**H2 `UnexpectedFrameType` 错误日志**：底层 `xitca-http 0.7.1` 的已知 bug，浏览器取消 H2 请求时触发，**不影响功能**。临时规避：

```toml
log_level = "sweety_lib=info,xitca_http=off"
```

---

## License

MIT
