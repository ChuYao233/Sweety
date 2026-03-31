# Sweety

高性能、单文件部署的多站点 Web 服务器，纯 Rust 编写。

底层 HTTP 栈原基于 [xitca-web](https://github.com/HFQR/xitca-web)，现已完整 fork 到 `vendor/` 目录自行维护（sweety-web / sweety-http-core / sweety-server / sweety-io 等），与上游独立演进，包含多项针对生产场景的性能修复和优化。

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

### 子命令（类 Caddy 风格）

```
Usage: sweety [OPTIONS] [COMMAND]

Commands:
  run       在前台启动 Sweety 并持续运行（推荐生产使用）
  validate  验证配置文件语法和 TLS 证书（不启动服务，等价 nginx -t）
  reload    向运行中的 Sweety 热重载配置（不断连应用）
  api-doc   输出 Admin REST API 接口文档 JSON（面板对接用）
  version   输出版本信息

Options:
  -c, --config <FILE>  配置文件路径 [default: config/sweety.toml]
  -h, --help           Print help
  -V, --version        Print version
```

```bash
# 启动（默认读 config/sweety.toml）
sweety run

# 指定配置文件
sweety run --config /etc/sweety/sweety.toml

# 验证配置 + TLS 证书（部署前必用）
sweety validate
sweety validate --config /etc/sweety/sweety.toml

# 热重载（不断开现有连接）
sweety reload

# 查看 Admin API 文档
sweety api-doc
sweety api-doc | jq '.endpoints[] | {method,path,description}'

# 版本
sweety version
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

### 反向代理（含超时/重试/断路器）

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

# 超时控制（秒）
connect_timeout = 5   # 连接上游超时（默认 10）
read_timeout    = 30  # 读取响应超时（默认 60）
write_timeout   = 30  # 写入请求超时（默认 60）

# 失败重试
retry         = 2  # 失败后最多重试 2 次（默认 0）
retry_timeout = 1  # 重试前等待 1 秒（默认 0 = 立即）

# 断路器（类 Nginx max_fails/fail_timeout，支持全局开关）
[sites.upstreams.circuit_breaker]
max_failures = 5   # 60 秒窗口内失败 5 次则开路
window_secs  = 60
fail_timeout = 30  # 开路后 30 秒尝试半开探测

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

### 插件系统（`handler = "plugin:xxx"`）

Sweety 内置插件接入能力，可在请求/响应两个阶段挂载自定义逻辑（WAF、自定义认证、限流扩展等）：

```toml
[[sites.locations]]
path    = "/api/"
handler = "plugin:my_waf"   # 格式：plugin:<name>
```

**实现插件（Rust）：**

```rust
use sweety_lib::handler::plugin::{Plugin, PluginRequest, PluginResult, plugin_registry};
use std::sync::Arc;

struct MyWaf;

impl Plugin for MyWaf {
    fn name(&self) -> &'static str { "my_waf" }

    fn on_request(&self, req: &PluginRequest<'_>) -> PluginResult {
        if req.headers.get("x-bad-header").is_some() {
            return PluginResult::Stop(forbidden_response());
        }
        PluginResult::Continue
    }
}

// main.rs 启动时注册
plugin_registry().register("my_waf", Arc::new(MyWaf));
```

**已注册插件查询：**
```bash
curl http://127.0.0.1:9000/api/v1/plugins
```

完整配置参考 [`config/sweety.example.toml`](config/sweety.example.toml)。

---

## Admin REST API

Sweety 内置 HTTP 管理 API，用于面板对接、热重载、监控采集。

### 配置

```toml
[global]
admin_listen = "127.0.0.1:9000"  # 管理 API 监听地址（建议只绑定 lo）
admin_token  = "your-secret"     # Bearer Token（为空则不鉴权）
```

### 认证

所有标记 `auth_required: true` 的接口需要 Bearer Token：

```bash
curl -H "Authorization: Bearer your-secret" http://127.0.0.1:9000/api/v1/stats
```

### 接口列表

| 方法 | 路径 | 鉴权 | 说明 |
|------|------|------|------|
| `GET` | `/api/v1/health` | 否 | 健康检查 |
| `GET` | `/api/v1/version` | 否 | 版本信息 |
| `GET` | `/api/v1/doc` | 否 | 本 API 文档（JSON） |
| `GET` | `/api/v1/stats` | 是 | 全局请求统计快照 |
| `GET` | `/api/v1/sites` | 是 | 站点列表 |
| `GET` | `/api/v1/upstreams` | 是 | 上游节点 + 断路器状态 |
| `POST` | `/api/v1/upstreams/:name/nodes/:addr/enable` | 是 | 启用节点 |
| `POST` | `/api/v1/upstreams/:name/nodes/:addr/disable` | 是 | 禁用节点（手动熔断） |
| `POST` | `/api/v1/reload` | 是 | 热重载配置（不断连） |
| `GET` | `/api/v1/plugins` | 是 | 已注册插件列表 |

### 接口详情

#### `GET /api/v1/health`
```json
{ "status": "ok" }
```

#### `GET /api/v1/version`
```json
{ "name": "Sweety", "version": "0.1.0" }
```

#### `GET /api/v1/stats`
```json
{
  "total_requests":    12345,
  "active_connections": 42,
  "bytes_sent":        9876543,
  "bytes_received":    1234567,
  "error_4xx":         88,
  "error_5xx":         3
}
```

#### `GET /api/v1/upstreams`
```json
{
  "upstreams": [{
    "name": "backend",
    "nodes": [{
      "addr":                "127.0.0.1:8001",
      "healthy":             true,
      "active_connections":  5,
      "circuit_breaker_open": false
    }]
  }]
}
```

#### `POST /api/v1/reload`
```bash
curl -X POST -H "Authorization: Bearer your-secret" \
  http://127.0.0.1:9000/api/v1/reload
```
```json
{ "success": true }
```

#### `GET /api/v1/doc`
返回本文档的机器可读 JSON 版本（同 `sweety api-doc` 输出）。

### 命令行快速查看文档

```bash
# 输出完整 API 文档 JSON
sweety api-doc

# 只看所有接口路径
sweety api-doc | jq '.endpoints[] | "\(.method) \(.path)"'

# 只看需要鉴权的接口
sweety api-doc | jq '.endpoints[] | select(.auth_required) | .path'
```

---

## 项目结构

```
sweety/
├─ src/
│  ├─ main.rs              # CLI 子命令入口（run/validate/reload/api-doc/version）
│  ├─ server/
│  │  ├─ http.rs           # 应用构建、多站点分发、AppState
│  │  ├─ tls.rs            # Rustls SNI Resolver、ACME HTTP-01 自动续期
│  │  ├─ dns01.rs          # ACME DNS-01（Cloudflare/Aliyun/Shell）通配符证书
│  │  └─ quic.rs           # HTTP/3 Quinn 集成
│  ├─ dispatcher/
│  │  ├─ vhost.rs          # 虚拟主机注册表（ArcSwap 无锁热更新）
│  │  ├─ location.rs       # Location 优先级匹配、per-location conn_count
│  │  └─ rewrite.rs        # Rewrite 规则引擎
│  ├─ handler/
│  │  ├─ static_file.rs    # 静态文件（内存缓存 + notify 失效 + Range + br/gz）
│  │  ├─ sendfile.rs       # Zero-copy 传输（Linux sendfile + H2 背压 stream）
│  │  ├─ fastcgi.rs        # FastCGI 协议实现 + 连接池
│  │  ├─ reverse_proxy/
│  │  │  ├─ mod.rs         # 反向代理主逻辑（retry 循环）
│  │  │  ├─ lb.rs          # 负载均衡 + 节点状态 + 断路器集成
│  │  │  ├─ circuit_breaker.rs  # 断路器（无锁原子三状态机）
│  │  │  ├─ conn.rs        # HTTP 连接层（超时控制 + keepalive 池）
│  │  │  ├─ pool.rs        # TCP/TLS 连接池
│  │  │  ├─ response.rs    # 响应头透传、Cookie/Location 改写
│  │  │  ├─ tls_client.rs  # 上游 TLS 客户端
│  │  │  └─ ws_proxy.rs    # WebSocket 代理
│  │  ├─ plugin/
│  │  │  └─ mod.rs         # 插件系统（trait + 全局注册表 + 生命周期钩子）
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
├─ vendor/                 # 自主维护的底层 HTTP 栈（sweety-web/http-core/server/io 等）
├─ scripts/
│  └─ sysctl_tune.sh       # Linux 内核参数调优脚本
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
| `[sites.tls]` | `acme` `acme_email` `acme_provider` `acme_challenge`(`http01`/`dns01`) `acme_renew_days_before` `cert` `key` `certs[]` `min_version` `max_version` |
| `[sites.hsts]` | `max_age` `include_sub_domains` `preload` |
| `[sites.fastcgi]` | `socket` `host` `port` `pool_size` `connect_timeout` `read_timeout` `cache{}` |
| `[sites.proxy_cache]` | `path` `max_entries` `ttl` `cacheable_statuses` `cacheable_methods` `bypass_headers` |
| `[[sites.upstreams]]` | `name` `strategy` `keepalive` `keepalive_requests` `keepalive_time` `connect_timeout` `read_timeout` `write_timeout` `retry` `retry_timeout` `circuit_breaker{max_failures,window_secs,fail_timeout}` `health_check{}` `nodes[]{addr,weight,tls,tls_sni,upstream_host}` |
| `[[sites.rewrites]]` | `pattern` `target` `flag`（last/break/redirect/permanent）`condition`（!-f/!-d）|
| `[sites.rate_limit]` | `rules[]{dimension,rate,burst,nodelay,path_pattern,header_name}` |
| `[[sites.locations]]` | `path` `handler`(`static`/`reverse_proxy`/`fastcgi`/`grpc`/`plugin:xxx`) `upstream` `root` `try_files` `return_code` `return_url` `return_body` `return_content_type` `limit_conn` `proxy_buffering` `cache_control` `auth_request` `auth_failure_status` `proxy_set_headers[]` `add_headers[]` `sub_filter[]` `cache_rules[]` |

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
| ACME 自动证书 HTTP-01 | ✅ | ❌ 需 certbot | ✅ |
| ACME DNS-01 / 通配符证书 | ✅ Cloudflare/Aliyun/Shell | ❌ 需 certbot | ✅ |
| Brotli 压缩内置 | ✅ | ❌ 第三方模块 | ✅ |
| FastCGI 连接池 | ✅ | ✅ | ✅ |
| auth_request | ✅ | ✅ | ❌ 需插件 |
| gRPC 代理 | ✅ | ✅ | ✅ |
| WebSocket 代理 | ✅ | ✅ | ✅ |
| respond 直接返回内容 | ✅ | ❌ 仅状态码 | ✅ |
| 反向代理超时/重试 | ✅ | ✅ | ✅ |
| 断路器 circuit breaker | ✅ 三状态机 | ⚠️ max_fails 仅计数 | ❌ |
| 零拷贝大文件传输 | ✅ sendfile(2) / H2 1MB 帧 | ✅ sendfile | ⚠️ |
| 插件系统 | ✅ `plugin:xxx` | ✅ C 模块 | ✅ Go 模块 |
| 子命令 CLI | ✅ run/validate/reload/api-doc | ⚠️ nginx -t/-s | ✅ |
| Admin REST API | ✅ | ❌ | ✅ |
| 配置热重载不断连 | ✅ | ✅ reload | ✅ |
| 单文件无依赖 | ✅ | ❌ | ✅ |
| 内存安全 | ✅ Rust | ❌ C | ✅ Go |
| GC 暂停 | ✅ 无 | ✅ 无 | ⚠️ Go GC |
| `if` 条件块 / `map` 变量 | ❌ | ✅ | ✅ |
| TCP/UDP 四层代理 | ❌ | ✅ stream | ✅ layer4 |

---

## 性能调优

### Linux 内核参数（生产必配）

```bash
sudo bash scripts/sysctl_tune.sh
```

脚本自动配置：TCP BBR 拥塞控制、TCP Fast Open、SO_REUSEPORT、收发缓冲区 128MB、somaxconn 65535、文件描述符 1048576。

撤销：`sudo bash scripts/sysctl_tune.sh restore`

### 性能关键参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| H2 连接窗口 | 128MB | 高带宽下不因流控停顿 |
| H2 流窗口 | 16MB | 单个大文件流无停顿 |
| H2 最大帧 | 1MB | 大文件调度开销降低 64 倍 |
| H2 发送缓冲 | 16MB | 生产者不频繁等待 |
| 文件流 chunk | 1MB | HTTPS 大文件带宽利用率提升 |
| 文件流 channel | 32 | 在途数据 32MB，覆盖千兆 RTT |
| TLS session cache | 65536 | 减少重复握手 |
| TCP_NODELAY | 默认开 | 响应头立即发出，降低 TTFB |
| SO_RCVBUF/SNDBUF | 1MB | 内核 socket 缓冲区 |

---

## License

MIT
