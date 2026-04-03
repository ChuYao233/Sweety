# 性能测试

## 测试环境

| 项目 | 规格 |
|------|------|
| 服务器 | 4 核 4GB / 5Mbps 带宽 |
| OS | Linux x86_64 |
| Rust | 1.75+ release build |
| 对比 | Nginx 1.24（标准配置） |

---

## 静态文件（HTTPS/H2，10KB）

### 测试命令

```bash
wrk -t4 -c1000 -d30s https://your.domain.com/10k.bin
```

### 结果

| 并发 | Sweety RPS | Sweety 吞吐 | Nginx RPS | Nginx 吞吐 | 备注 |
|------|-----------|-------------|-----------|------------|------|
| 1 | — | 104 MB/s | — | 89 MB/s | |
| 10 | — | 125 MB/s | — | 141 MB/s | |
| 100 | — | 159 MB/s | — | 134 MB/s | |
| 1000 | 12,273 | 122 MB/s | 25,926* | 250 MB/s | *Nginx 有大量 GOAWAY 错误 |
| 5000 | 22,861 | 227 MB/s | — | — | |
| 6000 | 22,872 | 226 MB/s | — | — | 无错误 |
| 8000 | 21,849 | 217 MB/s | — | — | 轻微退化 |

> Nginx 1000 并发数据存在 GOAWAY 错误（HTTP/2 连接重建后计入吞吐），对比不完全公平。
> Sweety 在测试范围内**零错误**。

### P99 延迟对比（1000并发）

| | Sweety | Nginx |
|---|---|---|
| P50 | 46ms | — |
| P90 | — | — |
| P99 | 73ms | 697ms |

Sweety P99 尾延迟比 Nginx **低约 10 倍**。

---

## HTTPS/H2 吞吐峰值（100KB 文件）

| 并发 | Sweety | Nginx |
|------|--------|-------|
| 1 | 104 MB/s | 89 MB/s |
| 10 | 125 MB/s | 141 MB/s |
| 100 | 159 MB/s | 134 MB/s |
| 1000 | 128 MB/s | 250 MB/s* |

---

## HTTP/3 支持

HTTP/3（QUIC）原生支持，与 H2 共享 443 端口，高丢包/高延迟网络提升显著：

```bash
curl -I --http3 https://your.domain.com
```

---

## PHP/WordPress 性能

### 测试场景

- WordPress 首页，启用 FastCGI 缓存
- 服务器：4核4GB，5Mbps 带宽
- PHP-FPM：pm.max_children = 50，OPcache 开启

### 不缓存 vs 开启缓存

| | 无缓存 | FastCGI 缓存（ttl=300s） |
|---|---|---|
| 并发 10 RPS | ~10 | ~200+ |
| P99 延迟 | 500ms+ | <20ms |

---

## 性能调优建议

### 1. 启用 jemalloc（高并发场景 +10%）

```bash
cargo build --release --features jemalloc
```

### 2. 优化 [global] 配置

```toml
[global]
worker_threads     = 0       # 自动 = CPU 核心数
worker_connections = 51200
keepalive_timeout  = 60

# HTTP/2 调优
h2_max_concurrent_streams = 256
h2_max_frame_size         = 65535
h2_max_requests_per_conn  = 2000
```

### 3. 静态文件缓存行为

- 文件 < 512KB：自动内存缓存，热请求零磁盘 I/O
- 文件 > 512KB：sendfile 零拷贝
- 缓存键：文件路径（canonicalize 前后双键，fast path 跳过 stat syscall）

### 4. FastCGI 连接池

```toml
[sites.fastcgi]
pool_size = 35   # PHP-FPM pm.max_children 的 70%
```

### 5. 系统级优化

```bash
# 增大文件描述符
echo "* soft nofile 65535" >> /etc/security/limits.conf
echo "* hard nofile 65535" >> /etc/security/limits.conf

# TCP 参数
sysctl -w net.core.somaxconn=65535
sysctl -w net.ipv4.tcp_max_syn_backlog=65535
sysctl -w net.ipv4.tcp_fin_timeout=15
sysctl -w net.ipv4.tcp_keepalive_time=300
```

### 6. HTTP/3 QUIC 窗口调优（高带宽场景）

```toml
[sites.tls.http3]
receive_window        = 16777216   # 16MB（默认 8MB）
stream_receive_window = 4194304    # 4MB（默认 2MB）
send_window           = 16777216   # 16MB（默认 8MB）
```

---

## 与 Nginx/Caddy 特性对比

| 特性 | Sweety | Nginx | Caddy |
|------|--------|-------|-------|
| 静态文件内存缓存 | ✅ LRU | ✅（OS page cache） | ❌ |
| FastCGI 响应缓存 | ✅ | ✅ fastcgi_cache | ❌ |
| HTTP/3 | ✅ 原生 | ⚠️ 需 patch/商业版 | ✅ |
| 零错误高并发 | ✅ | ⚠️ GOAWAY 错误 | — |
| P99 尾延迟 | 优秀 | 较高 | — |
| 内存安全 | ✅ Rust | ❌ C | ✅ Go |

---

## 压测工具

```bash
# wrk（HTTP/1.1 + H2）
wrk -t4 -c1000 -d30s -H "Connection: keep-alive" https://your.domain.com/

# hey
hey -n 100000 -c 1000 https://your.domain.com/

# k6
k6 run --vus 1000 --duration 30s script.js
```
