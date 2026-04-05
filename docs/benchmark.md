# 性能测试

**Sweety** 与 **Nginx** 在 HTTP/1.1、HTTP/2、HTTP/3 (QUIC) 下的全面性能对比。

| 项目 | 详情 |
|------|------|
| **CPU** | Intel Celeron J4105 @ 1.50GHz（**2 核心**） |
| **内存** | 1 GB |
| **链路带宽** | 2.5 Gbps（TLS 实际上限约 **270 MB/s**） |
| **OS** | Debian Linux |
| **TLS** | TLSv1.3, ECDSA P-256 |
| **Sweety** | 0.2.0 |
| **Nginx** | 1.29.7 |
| **工具** | [h2load](https://nghttp2.org/documentation/h2load.1.html) · 每次 15 秒 |

> **空闲内存**：Sweety **8.65 MB** vs Nginx 75.34 MB（**−88%**）

---

## HTTP/1.1 (HTTPS TLSv1.3)

| 文件 | 并发 | 服务器 | RPS | 带宽 MB/s | P50 | P95 | P99 | 内存 MB | CPU% | Δ RPS | 备注 |
|------|------|--------|-----|-----------|-----|-----|-----|---------|------|-------|------|
| **1 KB** | 1000 | **Sweety** | **106,695** | **134.3** | **91ms** | **113ms** | **137ms** | **50.5** | 100% | **+477%** | CPU 瓶颈 |
| | | Nginx | 18,480 | 23.9 | 524ms | 564ms | 691ms | 134.4 | 100% | | CPU 瓶颈 |
| **10 KB** | 1000 | **Sweety** | **13,655** | **137.3** | **504ms** | **1.78s** | **2.89s** | **50.8** | 42% | **+12%** | |
| | | Nginx | 12,187 | 122.9 | 602ms | 1.85s | 3.02s | 158.9 | 70% | | |
| **100 KB** | 1000 | **Sweety** | **1,520** | **152.3** | **4.73s** | **8.75s** | **10.43s** | **149.6** | 54% | **+9%** | |
| | | Nginx | 1,397 | 139.6 | 5.13s | 9.13s | 10.92s | 246.0 | 42% | | |
| **1 MB** | 100 | **Sweety** | **236.6** | **240.0** | **3.72s** | **5.00s** | **6.95s** | **28.0** | 56% | **+22%** | 接近带宽上限 |
| | | Nginx | 194.1 | 197.5 | 4.14s | 6.92s | 8.13s | 140.0 | 30% | | |
| **10 MB** | 10 | Sweety | 26.53 | 268.7 | 3.69s | **3.76s** | **3.80s** | **17.5** | 67% | **=** | **🔗 带宽瓶颈** |
| | | Nginx | 26.73 | 271.1 | **3.64s** | 3.87s | 4.30s | 89.6 | 40% | | **🔗 带宽瓶颈** |

---

## HTTP/2 (HTTPS TLSv1.3)

| 文件 | 并发 | 服务器 | RPS | 带宽 MB/s | P50 | P95 | P99 | 内存 MB | CPU% | Δ RPS | 备注 |
|------|------|--------|-----|-----------|-----|-----|-----|---------|------|-------|------|
| **1 KB** | 1000 | **Sweety** | **27,276** | **27.9** | **357ms** | **374ms** | **394ms** | **75.9** | 100% | **+48%** | CPU 瓶颈 |
| | | Nginx | 18,479 | 21.5 | 508ms | 669ms | 853ms | 134.0 | 100% | | CPU 瓶颈 |
| **10 KB** | 1000 | **Sweety** | **14,148** | **138.9** | **462ms** | 1.72s | 2.83s | **72.0** | 63% | **+8%** | |
| | | Nginx | 13,061 | 130.1 | 579ms | **1.68s** | **2.77s** | 158.0 | 75% | | |
| **100 KB** | 1000 | **Sweety** | **2,320** | **236.8** | 3.33s | 5.29s | 6.90s | 340.5 | 100% | **+799%** | Nginx 72% 连接停滞¹ |
| | | Nginx¹ | 258 | 27.7 | **1.50s** | **2.57s** | **2.67s** | **250.7** | 35% | | 仅完成 3864 请求 |
| **1 MB** | 100 | **Sweety** | **214.9** | **251.1** | **3.70s** | **5.88s** | 8.63s | 428.1 | 45% | **+7%** | 接近带宽上限 |
| | | Nginx | 201.8 | 221.4 | 3.95s | 6.19s | **7.08s** | 615.0 | 50% | | |
| **10 MB** | 10 | Sweety | 22.20 | 265.4 | 3.76s | 4.20s | 4.23s | **64.5** | 42% | **−11%** | **🔗 带宽瓶颈** |
| | | Nginx | **24.93** | **268.3** | **3.68s** | **4.11s** | **4.14s** | 137.7 | 47% | | **🔗 带宽瓶颈** |

> ¹ **H2 100KB×1000**：Nginx 仅完成 3,864 请求（258 req/s），Sweety 完成 34,797 请求（2,320 req/s）。Nginx P99 更低是因为在途请求更少，非处理更快。约 72% 的 Nginx 连接处于停滞/排队状态。

---

## HTTP/3 QUIC

| 文件 | 并发 | 服务器 | RPS | 带宽 MB/s | P50 | P95 | P99 | 内存 MB | CPU% | Δ RPS | 备注 |
|------|------|--------|-----|-----------|-----|-----|-----|---------|------|-------|------|
| **1 KB** | 1000 | **Sweety** | **33,104** | **36.9** | **100ms** | **898ms** | **2.30s** | 427.2 | 100% | **+115%** | CPU 瓶颈 |
| | | Nginx | 15,411 | 18.0 | 170ms | 1.18s | 3.19s | **365.0** | 100% | | CPU 瓶颈 |
| **10 KB** | 1000 | **Sweety** | **14,638** | **145.4** | **139ms** | **1.60s** | **4.34s** | **348.4** | 100% | **+163%** | |
| | | Nginx | 5,564 | 55.4 | 335ms | 3.00s | 6.47s | 374.7 | 100% | | |
| **100 KB** | 1000 | **Sweety** | **1,778** | **181.6** | **1.53s** | **8.15s** | 12.61s | 625.2 | 100% | **+143%** | |
| | | Nginx | 733 | 73.5 | 3.31s | 8.94s | **10.42s** | **908.0** | 100% | | |
| **1 MB** | 100 | **Sweety** | **209.7** | **217.1** | **1.08s** | **1.58s** | **1.95s** | **204.3** | 100% | **+206%** | |
| | | Nginx | 68.5 | 82.2 | 9.76s | 13.01s | 13.94s | 672.1 | 100% | | |
| **10 MB** | 10 | **Sweety** | **20.27** | **216.2** | **3.69s** | **6.79s** | **7.37s** | **238.4** | 100% | **+271%** | 接近带宽上限 |
| | | Nginx | 5.47 | 82.1 | 12.47s | 13.87s | 14.88s | 672.1 | 100% | | |

---

## 分析与结论

### Sweety 优势

**1. 小文件高并发吞吐碾压**

H1 1KB 场景下 107K vs 18K RPS（**+477%**），P99 仅 137ms vs 691ms。H2 1KB 同样领先 48%，P95–P99 区间仅 374–394ms vs 669–853ms。根本原因：tokio 异步运行时在海量短请求场景下调度开销远低于 Nginx 的 epoll + worker 进程模型。

**2. HTTP/3 全场景碾压**

H3 从 1KB 到 10MB 全面领先 115%–271%，文件越大差距越大：10MB 场景 Sweety 20.3 RPS / 216 MB/s vs Nginx 5.47 RPS / 82 MB/s（**+271%**）。1MB 场景 P99 仅 1.95s vs Nginx 13.94s，内存仅 204 MB vs 672 MB。Sweety 基于 quinn/h3 的 QUIC 实现在 UDP 多路复用、拥塞控制（BBR）、backpressure 上效率远高于 Nginx 的 QUIC 实现。0.2.0 版本引入连接级内存限流（`max_handlers`），彻底解决了高并发大文件传输的 OOM 问题。

**3. 内存效率**

空闲占用 8.65 MB vs 75.34 MB（**−88%**）。多数负载场景下内存占用减少 44–79%。H3 1MB 场景 204 MB vs 672 MB（−70%），H2 1MB 场景 428 MB vs 615 MB（−30%）。

**4. 尾延迟控制**

H1/H2 小文件场景 P95–P99 区间极窄，标准差远低于 Nginx。H3 1MB 场景 P99 仅 1.95s（Nginx 13.94s），得益于连接级 backpressure 和 pread_stream 全局信号量的双重控制。

**5. 零错误**

全部测试场景零请求失败、零超时。Nginx 在 H2 100KB×1000 场景下 72% 连接停滞（仅完成 3,864 请求 vs Sweety 34,797 请求）。

**6. 协议覆盖**

同一进程同时提供 H1 + H2 + H3，无需额外编译模块。Nginx 的 HTTP/3 需要重新编译并且性能明显更差。

### Nginx 优势

**1. 生态与生产验证**

20 年生产检验、海量文档、成熟的第三方模块生态（WAF、Lua、OpenResty 等），全球大规模部署的运维经验和工具链。遇到问题时有大量社区资源和专业支持渠道可求助。

**2. 四层代理**

Nginx `stream {}` 模块支持 TCP/UDP 四层代理，可代理数据库、SSH 等任意 TCP 协议。Sweety 尚未实现此功能。

### Sweety 当前不成熟之处

| 方面 | 现状 | 说明 |
|------|------|------|
| **生产验证** | ⚠️ 未经生产环境检验 | 未在真实高流量环境下长期运行，可靠性、边界场景、内存泄漏等尚无充分验证 |
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
| **大型 CDN / 文件分发** | **Nginx** | 带宽瓶颈场景两者持平，Nginx 运维生态更成熟 |
| **需要 WAF / Lua 扩展** | **Nginx** | OpenResty / ModSecurity 等成熟安全生态 |
| **TCP/UDP 四层代理** | **Nginx** | Sweety 尚未支持 stream 模块 |
| **关键业务生产环境** | **Nginx** | 20 年生产验证，Sweety 尚未经过生产检验 |

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
max_handlers                = 0
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
