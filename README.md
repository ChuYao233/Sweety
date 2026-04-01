# Sweety

高性能、单文件部署的多站点 Web 服务器，纯 Rust 编写。

底层 HTTP 栈 fork 自 [xitca-web](https://github.com/HFQR/xitca-web)，自主维护于 `vendor/` 目录，包含多项生产场景性能修复和优化。

> **性能基准（4 核 VM）**：RPS 9728 vs Nginx 6209（+57%），P99 延迟 73ms vs Nginx 697ms，零错误。

---

## 文档 / Documentation

| 语言 | 文档 |
|------|------|
| 🇨🇳 中文 | [快速开始](docs/zh/快速开始.md) · [配置参考](docs/zh/配置参考.md) · [Roadmap](docs/zh/roadmap.md) |
| 🇺🇸 English | [Getting Started](docs/getting-started.md) · [Config Reference](docs/config-reference.md) |
| 📄 完整配置示例 | [config/sweety.example.toml](config/sweety.example.toml) |

---

## 特性速览

### 协议
- **HTTP/1.1 + HTTP/2 + HTTP/3（QUIC）** 同一进程同时监听
- **TLS**：Rustls 纯 Rust，无 OpenSSL 依赖；多证书 SNI 自动选最优
- **ACME 自动证书**：HTTP-01 + DNS-01，支持 Let's Encrypt / ZeroSSL / Buypass，通配符证书；自签名占位启动，申请成功后热重载（对标 Caddy）
- **WebSocket**：H1 Upgrade（RFC 6455）+ H2 extended CONNECT（RFC 8441）全透传

### 请求处理
- **静态文件**：内存缓存 + Range + ETag/Last-Modified + `try_files` + sendfile(2)
- **PHP/FastCGI**：Unix Socket / TCP 连接池 + `fastcgi_cache`；正确处理 HTTP/2 Cookie 合并（RFC 7540 §8.1.2.5），兼容 WordPress / Laravel 等主流框架
- **反向代理**：轮询 / 加权 / 最少连接 / IP 哈希 + 连接池 + 主动健康检查 + `proxy_cache`
- **gRPC 代理**：自动处理 `application/grpc` + Trailer
- **auth_request** 子请求鉴权

### 路由
- 虚拟主机（精确 / 通配符 / fallback 兜底）
- Location 四级优先级：`= 精确` > `^~ 前缀优先` > `~ 正则` > `普通前缀`
- Rewrite 规则：正则捕获，`last/break/redirect/permanent`，`!-f/!-d` 条件

### 性能架构
- **SO_REUSEPORT 多核扩展**：每个 worker 线程独立 bind 同一端口，内核负载均衡，无锁竞争
- **H2 per-connection writer loop**：每连接单独 writer task，HEADERS 优先 + round-robin DATA 调度，消除 head-of-line blocking
- **write fairness**：固定 16KB chunk 轮转调度，防止大流下载饿死小请求
- **零 CPU 空转**：writer loop 基于 `tokio::select!` 事件驱动，无 busy spin

### 可靠性
- **断路器**：三状态机（Closed → Open → Half-Open），比 Nginx `max_fails` 更精确
- **五维度令牌桶限流**：IP / 路径 / IP+路径 / Header / User-Agent
- **配置热重载**：不断开现有连接，等价 `nginx -s reload`

### 运维
- **Admin REST API**：健康检查 / 统计 / 节点管理 / 热重载
- **访问日志**：combined / json / 自定义模板，异步写
- **Prometheus 指标**：`/metrics` 端点

---

## 快速编译 & 运行

```bash
cargo build --release

# 验证配置（等价 nginx -t）
./target/release/sweety validate

# 启动
./target/release/sweety run

# 热重载
./target/release/sweety reload
```

---

## 与 Nginx 对比

| 功能 | Sweety | Nginx |
|------|--------|-------|
| HTTP/3 内置 | ✅ | ❌ 需重新编译 |
| ACME 自动证书（Caddy 对标） | ✅ HTTP-01 + DNS-01，自签名占位启动 | ❌ 需 certbot |
| Brotli 压缩内置 | ✅ | ❌ 第三方模块 |
| WebSocket H2（RFC 8441） | ✅ | ✅ |
| 断路器 | ✅ 三状态机 | ⚠️ max_fails 仅计数 |
| H2 多核扩展（SO_REUSEPORT） | ✅ 每 worker 独立 bind | ✅ |
| Admin REST API | ✅ | ❌ |
| 单文件无依赖 | ✅ | ❌ |
| 内存安全 | ✅ Rust | ❌ C |
| `if` 条件块 / `map` 变量 | ❌ | ✅ |
| TCP/UDP 四层代理 | ❌ | ✅ stream |

与 Nginx 的差距跟踪：[docs/zh/roadmap.md](docs/zh/roadmap.md)

---

## License

MIT
