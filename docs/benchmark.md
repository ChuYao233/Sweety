# 性能测试

**Sweety** 与 **Nginx** 在 HTTP/1.1、HTTP/2、HTTP/3 (QUIC) 下的全面性能对比。

| 项目 | 详情 |
|------|------|
| **CPU** | Intel Celeron J4105 @ 1.50GHz（**2 核心**） |
| **内存** | 1 GB |
| **链路带宽** | 2.5 Gbps（TLS 实际上限约 **270 MB/s**） |
| **OS** | Debian Linux |
| **TLS** | TLSv1.3, ECDSA P-256 |
| **Sweety** | 0.1.0 (`70dce2e`) |
| **Nginx** | 1.29.7 |
| **工具** | [h2load](https://nghttp2.org/documentation/h2load.1.html) · 每次 15 秒 |

> **空闲内存**：Sweety **8.65 MB** vs Nginx 75.34 MB（**−88%**）

---

## HTTP/1.1 (HTTPS TLSv1.3)

| 文件 | 并发 | 服务器 | RPS | 带宽 MB/s | P50 | P95 | P99 | 内存 MB | CPU% | Δ RPS | 备注 |
|------|------|--------|-----|-----------|-----|-----|-----|---------|------|-------|------|
| **1 KB** | 1000 | **Sweety** | **107,524** | **138.4** | **91ms** | **98ms** | **114ms** | **49.9** | 100% | **+482%** | CPU 瓶颈 |
| | | Nginx | 18,480 | 23.9 | 524ms | 564ms | 691ms | 134.4 | 100% | | CPU 瓶颈 |
| **10 KB** | 1000 | **Sweety** | **13,347** | **134.5** | **513ms** | **1.81s** | 3.07s | **50.9** | 41% | **+10%** | |
| | | Nginx | 12,187 | 122.9 | 602ms | 1.85s | **3.02s** | 158.9 | 70% | | |
| **100 KB** | 1000 | **Sweety** | **1,702** | **169.9** | **4.47s** | **8.54s** | **10.35s** | 283.9 | 41% | **+22%** | |
| | | Nginx | 1,397 | 139.6 | 5.13s | 9.13s | 10.92s | **246.0** | 42% | | |
| **1 MB** | 100 | **Sweety** | **247.5** | **250.4** | **3.74s** | **4.81s** | **6.26s** | **69.0** | 41% | **+28%** | 接近带宽上限 |
| | | Nginx | 194.1 | 197.5 | 4.14s | 6.92s | 8.13s | 140.0 | 30% | | |
| **10 MB** | 10 | Sweety | 26.73 | 271.9 | 3.66s | **3.72s** | **3.78s** | **21.9** | 45% | **=** | **🔗 带宽瓶颈** |
| | | Nginx | 26.73 | 271.1 | **3.64s** | 3.87s | 4.30s | 89.6 | 40% | | **🔗 带宽瓶颈** |

---

## HTTP/2 (HTTPS TLSv1.3)

| 文件 | 并发 | 服务器 | RPS | 带宽 MB/s | P50 | P95 | P99 | 内存 MB | CPU% | Δ RPS | 备注 |
|------|------|--------|-----|-----------|-----|-----|-----|---------|------|-------|------|
| **1 KB** | 1000 | **Sweety** | **28,345** | **29.0** | **345ms** | **358ms** | **376ms** | **75.1** | 100% | **+53%** | CPU 瓶颈 |
| | | Nginx | 18,479 | 21.5 | 508ms | 669ms | 853ms | 134.0 | 100% | | CPU 瓶颈 |
| **10 KB** | 1000 | **Sweety** | **14,442** | **141.8** | **449ms** | 1.70s | 2.84s | **72.9** | 63% | **+11%** | |
| | | Nginx | 13,061 | 130.1 | 579ms | **1.68s** | **2.77s** | 158.0 | 75% | | |
| **100 KB** | 1000 | **Sweety** | **1,386** | **155.7** | 4.94s | 10.25s | 12.02s | 450.3 | 47% | **+437%** | Nginx 72% 连接停滞¹ |
| | | Nginx¹ | 258 | 27.7 | **1.50s** | **2.57s** | **2.67s** | **250.7** | 35% | | 仅完成 3864 请求 |
| **1 MB** | 100 | **Sweety** | **212.7** | **252.0** | **3.70s** | **5.61s** | 8.42s | **178.4** | 40% | **+5%** | 接近带宽上限 |
| | | Nginx | 201.8 | 221.4 | 3.95s | 6.19s | **7.08s** | 615.0 | 50% | | |
| **10 MB** | 10 | **Sweety** | **26.67** | 269.8 | 3.70s | **3.79s** | **3.82s** | **29.1** | 43% | **+7%** | **🔗 带宽瓶颈** |
| | | Nginx | 24.93 | 268.3 | **3.68s** | 4.11s | 4.14s | 137.7 | 47% | | **🔗 带宽瓶颈** |

> ¹ **H2 100KB×1000**：Nginx 仅完成 3,864 请求（258 req/s），Sweety 完成 20,788 请求（1,386 req/s）。Nginx P99 更低是因为在途请求更少，非处理更快。约 72% 的 Nginx 连接处于停滞/排队状态。

---

## HTTP/3 QUIC

| 文件 | 并发 | 服务器 | RPS | 带宽 MB/s | P50 | P95 | P99 | 内存 MB | CPU% | Δ RPS | 备注 |
|------|------|--------|-----|-----------|-----|-----|-----|---------|------|-------|------|
| **1 KB** | 1000 | **Sweety** | **28,901** | **32.5** | 298ms | **376ms** | **1.43s** | **363.4** | 100% | **+88%** | CPU 瓶颈 |
| | | Nginx | 15,411 | 18.0 | **170ms** | 1.18s | 3.19s | 365.0 | 100% | | CPU 瓶颈 |
| **10 KB** | 1000 | **Sweety** | **14,452** | **143.7** | **152ms** | **1.61s** | **4.03s** | **367.4** | 100% | **+160%** | |
| | | Nginx | 5,564 | 55.4 | 335ms | 3.00s | 6.47s | 374.7 | 100% | | |
| **100 KB** | 1000 | **Sweety** | **1,837** | **186.0** | **1.39s** | **6.51s** | 10.49s | **475.4** | 100% | **+151%** | |
| | | Nginx | 733 | 73.5 | 3.31s | 8.94s | **10.42s** | 908.0 | 100% | | |
| **1 MB** | 100 | **Sweety** | **186.7** | **203.8** | **2.18s** | **3.56s** | **4.56s** | **391.2** | 100% | **+173%** | |
| | | Nginx | 68.5 | 82.2 | 9.76s | 13.01s | 13.94s | 672.1 | 100% | | |
| **10 MB** | 10 | **Sweety** | **22.80** | **241.1** | **3.74s** | **6.04s** | **6.36s** | 230.4 | 100% | **+317%** | 接近带宽上限 |
| | | Nginx | 5.47 | 82.1 | 12.47s | 13.87s | 14.88s | **145.0** | 100% | | |

---

## 分析与结论

### Sweety 优势

**1. 小文件高并发吞吐碾压**

H1 1KB 场景下 107K vs 18K RPS（**+482%**），P99 仅 114ms vs 691ms。H2 1KB 同样领先 53%，P95–P99 区间仅 358–376ms vs 669–853ms，标准差 20ms vs 108ms。根本原因：tokio 异步运行时在海量短请求场景下调度开销远低于 Nginx 的 epoll + worker 进程模型。

**2. HTTP/3 全场景碾压**

H3 从 1KB 到 10MB 全面领先 88%–317%，文件越大差距越大：10MB 场景 Sweety 22.8 RPS / 241 MB/s vs Nginx 5.47 RPS / 82 MB/s（**+317%**）。Sweety 基于 quinn/h3 的 QUIC 实现在 UDP 多路复用、拥塞控制（BBR）、backpressure 上效率远高于 Nginx 的 QUIC 实现。

**3. 内存效率**

空闲占用 8.65 MB vs 75.34 MB（**−88%**）。多数负载场景下内存占用减少 44–79%。H3 100KB 场景 475 MB vs 908 MB（−48%），H2 1MB 场景 178 MB vs 615 MB（−71%）。

**4. 尾延迟控制**

H1/H2 小文件场景 P95–P99 区间极窄，标准差远低于 Nginx。H2 per-connection writer loop 配合 HEADERS 优先 + round-robin DATA 调度消除了 head-of-line blocking，使延迟分布更可预测。

**5. 零错误**

全部测试场景零请求失败、零超时。Nginx 在 H2 100KB×1000 场景下 72% 连接停滞（仅完成 3,864 请求 vs Sweety 20,788 请求）。

**6. 协议覆盖**

同一进程同时提供 H1 + H2 + H3，无需额外编译模块。Nginx 的 HTTP/3 需要重新编译并且性能明显更差。

### Nginx 优势

**1. sendfile(2) 内核零拷贝**

H1/H2 中等文件（100KB–1MB）场景下，Nginx 通过 `sendfile(2)` 直接从内核页缓存到 TLS 加密层，无需用户态拷贝。Sweety 必须 读取→用户态缓冲→TLS 加密→写入，导致内存占用更高（H2 100KB: 450 MB vs 250 MB）。

**2. 大文件稳定性**

H2 1MB 场景 Nginx P99 7.08s vs Sweety 8.42s，sendfile 路径下延迟更平稳。当文件体积足够大且链路带宽成为瓶颈时（10MB），两者 RPS 和吞吐接近持平。

**3. 生态与生产验证**

20 年生产检验、海量文档、成熟的第三方模块生态（WAF、Lua、OpenResty 等），全球大规模部署的运维经验和工具链。遇到问题时有大量社区资源和专业支持渠道可求助。

**4. 四层代理**

Nginx `stream {}` 模块支持 TCP/UDP 四层代理，可代理数据库、SSH 等任意 TCP 协议。Sweety 尚未实现此功能。

### Sweety 当前不成熟之处

| 方面 | 现状 | 说明 |
|------|------|------|
| **生产验证** | ⚠️ 未经生产环境检验 | 未在真实高流量环境下长期运行，可靠性、边界场景、内存泄漏等尚无充分验证 |
| **H2/TLS 大文件内存** | 用户态缓冲 | 缺少 `sendfile` 内核零拷贝路径，H2 100KB–1MB 高并发下内存占用高于 Nginx |
| **模块生态** | 基础插件系统 | 仅有 Rust trait 注册机制，无 Lua/WAF/OpenResty 等成熟生态 |
| **四层代理** | ❌ 未实现 | 不支持 `stream {}` 式的 TCP/UDP 透传 |
| **条件逻辑** | ❌ 无 `if` / `map` | 不支持配置层条件分支和变量映射 |
| **社区规模** | 极小 | 文档、教程、第三方集成均处于早期阶段 |
| **长期稳定性** | 未知 | 缺乏数月级别的持续高负载运行数据，GC-free 但 Rust unsafe 边界需持续审计 |

### 适用场景对比

| 场景 | 推荐 | 理由 |
|------|------|------|
| **API 网关 / 微服务入口** | **Sweety** | 5–6 倍 RPS，P99 < 120ms，内存仅 50 MB 级别 |
| **HTTP/3 部署** | **Sweety** | 全场景 2–4 倍 RPS，Nginx QUIC 实现性能差距明显 |
| **边缘节点 / 嵌入式** | **Sweety** | 8.65 MB 空闲占用，单二进制无依赖，适合资源受限环境 |
| **小型静态站点** | **Sweety** | 一行 preset 配置，ACME 自动证书，开箱即用 |
| **大型 CDN / 文件分发** | **Nginx** | sendfile 零拷贝 + 带宽瓶颈场景两者持平，Nginx 运维生态更成熟 |
| **需要 WAF / Lua 扩展** | **Nginx** | OpenResty / ModSecurity 等成熟安全生态 |
| **TCP/UDP 四层代理** | **Nginx** | Sweety 尚未支持 stream 模块 |
| **关键业务生产环境** | **Nginx** | 20 年生产验证，Sweety 尚未经过生产检验 |
| **中等文件极端并发** | **Nginx** | sendfile 路径内存占用更低且延迟更稳定 |

---

## 测试环境配置

### Sweety 配置

参见 [config/sweety.config.example](https://github.com/ChuYao233/Sweety/blob/main/config/sweety.config.example)

```toml
# ═══════════════════════════════════════════════════════════════════
# 全局配置
# ═══════════════════════════════════════════════════════════════════
[global]
worker_threads = 0
worker_connections = 65535
max_connections = 0
keepalive_timeout = 75
client_max_body_size = 0
client_header_buffer_size = 32
client_body_buffer_size = 512
gzip = false
log_level = "info"
h2_max_concurrent_streams = 128
h2_max_concurrent_reset_streams = 16384
h2_max_frame_size = 65535
h2_max_requests_per_conn = 10000
h2_max_pending_per_conn = 0

# ═══════════════════════════════════════════════════════════════════
# 站点配置
# ═══════════════════════════════════════════════════════════════════
[[sites]]
name        = "benchmark"
server_name = ["172.19.1.5"]
listen      = [80]
listen_tls  = [443]
root        = "/www/wwwroot/local"
index       = ["index.html", "index.htm"]
force_https = false
gzip        = false

[sites.tls]
cert        = "/www/cert/cert.pem"
key         = "/www/cert/key.pem"
min_version = "tls1.2"
max_version = "tls1.3"
protocols   = ["h3", "h2", "http/1.1"]

[sites.tls.http3]
enable_0rtt                 = true
max_concurrent_bidi_streams = 2000
receive_window              = 16777216
stream_receive_window       = 4194304
send_window                 = 16777216

[[sites.locations]]
path    = "/"
handler = "static"
```

### Nginx 配置

```nginx
# nginx.conf
daemon off;
master_process on;
user nginx;
worker_processes auto;
worker_rlimit_nofile 1048576;

events {
    use epoll;
    worker_connections 65535;
    multi_accept on;
}

http {
    include       mime.types;
    default_type  application/octet-stream;
    charset       off;
    server_tokens off;
    sendfile on;
    tcp_nopush on;
    tcp_nodelay on;
    aio threads;
    directio 8m;
    directio_alignment 4k;
    output_buffers 1 512k;
    keepalive_timeout 75;
    keepalive_requests 100000;
    reset_timedout_connection on;
    client_max_body_size 0;
    client_body_timeout 15s;
    send_timeout 300s;
    open_file_cache max=200000 inactive=120s;
    open_file_cache_valid 120s;
    open_file_cache_min_uses 1;
    open_file_cache_errors on;
    types_hash_max_size 4096;
    access_log off;
    log_not_found off;
    include /etc/nginx/conf.d/*.conf;
}
```

```nginx
# site.conf
server {
    listen 443 ssl;
    listen [::]:443 ssl;
    http2 on;
    listen 443 quic reuseport;
    listen [::]:443 quic reuseport;
    server_name 172.19.1.5;
    root /www/wwwroot/local;
    index index.html index.htm;
    ssl_certificate     /www/cert/cert.pem;
    ssl_certificate_key /www/cert/key.pem;
    ssl_protocols TLSv1.2 TLSv1.3;
    ssl_session_cache shared:SSL:50m;
    ssl_session_timeout 1d;
    ssl_session_tickets off;
    ssl_prefer_server_ciphers off;
    ssl_early_data on;
    add_header Alt-Svc 'h3=":443"; ma=86400' always;
    location / {
        try_files $uri $uri/ =404;
    }
    location ~* \.bin$ {
        default_type application/octet-stream;
        try_files $uri =404;
    }
}
```

---

## 复现测试结果

1. 准备测试文件：
```bash
dd if=/dev/urandom of=/www/wwwroot/local/1kb.bin bs=1K count=1
dd if=/dev/urandom of=/www/wwwroot/local/10kb.bin bs=10K count=1
dd if=/dev/urandom of=/www/wwwroot/local/100kb.bin bs=100K count=1
dd if=/dev/urandom of=/www/wwwroot/local/1mb.bin bs=1M count=1
dd if=/dev/urandom of=/www/wwwroot/local/10mb.bin bs=10M count=1
```

2. 运行基准测试：
```bash
# HTTP/1.1
h2load --duration=15 -c 1000 -m 10 -t 2 --h1 https://<host>/1kb.bin
h2load --duration=15 -c 1000 -m 10 -t 2 --h1 https://<host>/10kb.bin
h2load --duration=15 -c 1000 -m 10 -t 2 --h1 https://<host>/100kb.bin
h2load --duration=15 -c 100  -m 10 -t 2 --h1 https://<host>/1mb.bin
h2load --duration=15 -c 10   -m 10 -t 2 --h1 https://<host>/10mb.bin

# HTTP/2
h2load --duration=15 -c 1000 -m 10 -t 2 https://<host>/1kb.bin
h2load --duration=15 -c 1000 -m 10 -t 2 https://<host>/10kb.bin
h2load --duration=15 -c 1000 -m 10 -t 2 https://<host>/100kb.bin
h2load --duration=15 -c 100  -m 10 -t 2 https://<host>/1mb.bin
h2load --duration=15 -c 10   -m 10 -t 2 https://<host>/10mb.bin

# HTTP/3 (QUIC)
h2load --duration=15 -c 1000 -m 10 -t 2 --alpn-list=h3 https://<host>/1kb.bin
h2load --duration=15 -c 1000 -m 10 -t 2 --alpn-list=h3 https://<host>/10kb.bin
h2load --duration=15 -c 1000 -m 10 -t 2 --alpn-list=h3 https://<host>/100kb.bin
h2load --duration=15 -c 100  -m 10 -t 2 --alpn-list=h3 https://<host>/1mb.bin
h2load --duration=15 -c 10   -m 10 -t 2 --alpn-list=h3 https://<host>/10mb.bin
```

3. 测试期间监控资源：
```bash
# 在另一个终端中
watch -n 0.5 'ps -o pid,rss,%cpu,comm -p $(pgrep sweety)'
```
