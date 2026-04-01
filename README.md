# Sweety

高性能、单文件部署的多站点 Web 服务器，纯 Rust 编写。

底层 HTTP 栈 fork 自 [xitca-web](https://github.com/HFQR/xitca-web)，自主维护于 `vendor/` 目录，包含多项生产场景性能修复和优化。

> ## 性能基准
>
> **测试环境**：Intel J4105 四核四线程 @ 1.5GHz · 2GB RAM · Debian Linux
> **测试工具**：[bombardier](https://github.com/codesenberg/bombardier) v1.2.6
> **对比版本**：Nginx 1.28.1 vs Sweety v0.7.1
> **协议**：HTTPS/2，TLS 1.3，Keep-Alive，证书 RSA 2048
>
> **复现命令**：
> ```bash
> # 小文件高并发（1KB / 10KB / 100KB）
> bombardier -c 1000 -d 30s -k --latencies --http2 -t 30s https://<host>/1kb.bin
> # 中文件（1MB）
> bombardier -c 200 -d 30s -k --latencies --http2 -t 30s https://<host>/1mb.bin
> # 大文件（10MB）
> bombardier -c 100 -d 30s -k --latencies --http2 -t 30s https://<host>/10mb.bin
> ```
>
> ### 小文件高并发（1KB，-c 1000）
>
> | 指标 | Nginx 1.28.1 | Sweety | 优势 |
> |------|------------|--------|------|
> | RPS | ~6,227 | **15,133** | **+143%** |
> | 吞吐量 | 8.08 MB/s | **16.91 MB/s** | **+109%** |
> | P50 延迟 | 109.88ms | **65.12ms** | 节省 41% |
> | P90 延迟 | 284.93ms | **68.02ms** | **节省 76%** |
> | P95 延迟 | 530.32ms | **69.16ms** | **节省 87%** |
> | P99 延迟 | 1,360ms | **76.14ms** | **节省 94%** |
> | Stdev | 179.27ms | **7.16ms** | **极其平稳** |
> | 错误数 | ~20,467 (GOAWAY) | **0** | 零错误 |
>
> ### 小文件（10KB，-c 1000）
>
> | 指标 | Nginx 1.28.1 | Sweety | 优势 |
> |------|------------|--------|------|
> | RPS | 6,024 | **12,697** | **+111%** |
> | 吞吐量 | 56.07 MB/s | **126.26 MB/s** | **+125%** |
> | P50 延迟 | 123.47ms | **79.08ms** | 节省 36% |
> | P90 延迟 | 263.96ms | **81.85ms** | **节省 69%** |
> | P99 延迟 | 1,400ms | **86.70ms** | **节省 94%** |
> | Stdev | 160.47ms | **1.96ms** | **极其平稳** |
> | 错误数 | 17,903 (GOAWAY) | **0** | 零错误 |
>
> ### 中文件（100KB，-c 1000）
>
> | 指标 | Nginx 1.28.1 | Sweety | 差异 |
> |------|------------|--------|------|
> | RPS | **2,594** | 1,398 | Nginx +86% |
> | 吞吐量 | **253.14 MB/s** | 138.43 MB/s | Nginx 带宽更高 |
> | P50 延迟 | **397.73ms** | 613.71ms | — |
> | P99 延迟 | 719.53ms | 2,150ms | — |
> | 错误数 | 1,284 (GOAWAY) | **0** | Sweety 零错误 |
>
> ### 中文件（1MB，-c 200）
>
> | 指标 | Nginx 1.28.1 | Sweety | 差异 |
> |------|------------|--------|------|
> | RPS | **218.93** | 176.23 | Nginx +24% |
> | 吞吐量 | **216.55 MB/s** | 179.55 MB/s | Nginx 带宽更高 |
> | P50 延迟 | **0.90s** | 1.00s | 相近 |
> | P99 延迟 | 2.50s | **1.94s** | Sweety 更低 |
> | 错误数 | 167 (GOAWAY) | **0** | Sweety 零错误 |
>
> ### 大文件（10MB，-c 100）
>
> | 指标 | Nginx 1.28.1 | Sweety | 差异 |
> |------|------------|--------|------|
> | RPS | 14.53 | **16.75** | **+15%** |
> | 吞吐量 | 170.40 MB/s | **180.17 MB/s** | **+6%** |
> | P50 延迟 | 6.25s | **5.25s** | 节省 16% |
> | P99 延迟 | 8.28s | **7.88s** | 相近 |
> | 错误数 | 0 | **0** | 持平 |
>
> ### 结论
>
> | 场景 | 优势方 | 说明 |
> |------|--------|------|
> | **小文件（≤10KB）高并发** | **Sweety 大幅领先** | RPS +100%~+143%，延迟尾部低 94%，零 GOAWAY 错误 |
> | **大文件（≥10MB）** | **Sweety 持平/小幅领先** | 吞吐 +6%，延迟更低，内存恒定 |
> | **中文件（100KB~1MB）带宽压力** | Nginx 更高 | Nginx sendfile 内核路径优势，Sweety RPS 较低 |
>
> **Sweety 优势场景**：API 网关 / 小静态资源 CDN 分发 / 高并发低延迟要求 / 零运维（ACME 自动证书 + 热重载）
> **Nginx 优势场景**：百 KB 级大量并发下载（sendfile 内核优化），极端带宽压榨

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
