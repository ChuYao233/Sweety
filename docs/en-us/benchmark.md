# Performance Benchmark

**Sweety** vs **Nginx** across HTTP/1.1, HTTP/2, and HTTP/3 (QUIC).

| Item | Detail |
|------|--------|
| **CPU** | Intel Celeron J4105 @ 1.50GHz (**2 cores**) |
| **RAM** | 1 GB |
| **Link Bandwidth** | 2.5 Gbps (TLS practical ceiling ~**270 MB/s**) |
| **OS** | Debian Linux |
| **TLS** | TLSv1.3, ECDSA P-256 |
| **Sweety** | 0.2.0 |
| **Nginx** | 1.29.7 |
| **Tool** | [h2load](https://nghttp2.org/documentation/h2load.1.html) · 15s per run |

> **Idle memory**: Sweety **8.65 MB** vs Nginx 75.34 MB (**−88%**)

---

## HTTP/1.1 (HTTPS TLSv1.3)

| File | Conns | Server | RPS | BW MB/s | P50 | P95 | P99 | Mem MB | CPU% | Δ RPS | Note |
|------|-------|--------|-----|---------|-----|-----|-----|--------|------|-------|------|
| **1 KB** | 1000 | **Sweety** | **106,695** | **134.3** | **91ms** | **113ms** | **137ms** | **50.5** | 100% | **+477%** | CPU bound |
| | | Nginx | 18,480 | 23.9 | 524ms | 564ms | 691ms | 134.4 | 100% | | CPU bound |
| **10 KB** | 1000 | **Sweety** | **13,655** | **137.3** | **504ms** | **1.78s** | **2.89s** | **50.8** | 42% | **+12%** | |
| | | Nginx | 12,187 | 122.9 | 602ms | 1.85s | 3.02s | 158.9 | 70% | | |
| **100 KB** | 1000 | **Sweety** | **1,520** | **152.3** | **4.73s** | **8.75s** | **10.43s** | **149.6** | 54% | **+9%** | |
| | | Nginx | 1,397 | 139.6 | 5.13s | 9.13s | 10.92s | 246.0 | 42% | | |
| **1 MB** | 100 | **Sweety** | **236.6** | **240.0** | **3.72s** | **5.00s** | **6.95s** | **28.0** | 56% | **+22%** | Near BW limit |
| | | Nginx | 194.1 | 197.5 | 4.14s | 6.92s | 8.13s | 140.0 | 30% | | |
| **10 MB** | 10 | Sweety | 26.53 | 268.7 | 3.69s | **3.76s** | **3.80s** | **17.5** | 67% | **=** | **🔗 BW ceiling** |
| | | Nginx | 26.73 | 271.1 | **3.64s** | 3.87s | 4.30s | 89.6 | 40% | | **🔗 BW ceiling** |

---

## HTTP/2 (HTTPS TLSv1.3)

| File | Conns | Server | RPS | BW MB/s | P50 | P95 | P99 | Mem MB | CPU% | Δ RPS | Note |
|------|-------|--------|-----|---------|-----|-----|-----|--------|------|-------|------|
| **1 KB** | 1000 | **Sweety** | **27,276** | **27.9** | **357ms** | **374ms** | **394ms** | **75.9** | 100% | **+48%** | CPU bound |
| | | Nginx | 18,479 | 21.5 | 508ms | 669ms | 853ms | 134.0 | 100% | | CPU bound |
| **10 KB** | 1000 | **Sweety** | **14,148** | **138.9** | **462ms** | 1.72s | 2.83s | **72.0** | 63% | **+8%** | |
| | | Nginx | 13,061 | 130.1 | 579ms | **1.68s** | **2.77s** | 158.0 | 75% | | |
| **100 KB** | 1000 | **Sweety** | **2,320** | **236.8** | 3.33s | 5.29s | 6.90s | 340.5 | 100% | **+799%** | Nginx 72% stalled¹ |
| | | Nginx¹ | 258 | 27.7 | **1.50s** | **2.57s** | **2.67s** | **250.7** | 35% | | Only 3864 reqs |
| **1 MB** | 100 | **Sweety** | **214.9** | **251.1** | **3.70s** | **5.88s** | 8.63s | 428.1 | 45% | **+7%** | Near BW limit |
| | | Nginx | 201.8 | 221.4 | 3.95s | 6.19s | **7.08s** | 615.0 | 50% | | |
| **10 MB** | 10 | Sweety | 22.20 | 265.4 | 3.76s | 4.20s | 4.23s | **64.5** | 42% | **−11%** | **🔗 BW ceiling** |
| | | Nginx | **24.93** | **268.3** | **3.68s** | **4.11s** | **4.14s** | 137.7 | 47% | | **🔗 BW ceiling** |

> ¹ **H2 100KB×1000**: Nginx completed only 3,864 requests (258 req/s) vs Sweety's 34,797 (2,320 req/s). Nginx's lower P99 reflects fewer in-flight requests, not faster processing. ~72% of Nginx connections were stalled/queued.

---

## HTTP/3 QUIC

| File | Conns | Server | RPS | BW MB/s | P50 | P95 | P99 | Mem MB | CPU% | Δ RPS | Note |
|------|-------|--------|-----|---------|-----|-----|-----|--------|------|-------|------|
| **1 KB** | 1000 | **Sweety** | **33,104** | **36.9** | **100ms** | **898ms** | **2.30s** | 427.2 | 100% | **+115%** | CPU bound |
| | | Nginx | 15,411 | 18.0 | 170ms | 1.18s | 3.19s | **365.0** | 100% | | CPU bound |
| **10 KB** | 1000 | **Sweety** | **14,638** | **145.4** | **139ms** | **1.60s** | **4.34s** | **348.4** | 100% | **+163%** | |
| | | Nginx | 5,564 | 55.4 | 335ms | 3.00s | 6.47s | 374.7 | 100% | | |
| **100 KB** | 1000 | **Sweety** | **1,778** | **181.6** | **1.53s** | **8.15s** | 12.61s | 625.2 | 100% | **+143%** | |
| | | Nginx | 733 | 73.5 | 3.31s | 8.94s | **10.42s** | **908.0** | 100% | | |
| **1 MB** | 100 | **Sweety** | **209.7** | **217.1** | **1.08s** | **1.58s** | **1.95s** | **204.3** | 100% | **+206%** | |
| | | Nginx | 68.5 | 82.2 | 9.76s | 13.01s | 13.94s | 672.1 | 100% | | |
| **10 MB** | 10 | **Sweety** | **20.27** | **216.2** | **3.69s** | **6.79s** | **7.37s** | **238.4** | 100% | **+271%** | Near BW limit |
| | | Nginx | 5.47 | 82.1 | 12.47s | 13.87s | 14.88s | 672.1 | 100% | | |

---

## Analysis & Conclusions

### Sweety Advantages

**1. Small-file high-concurrency throughput dominance**

H1 1KB: 107K vs 18K RPS (**+477%**), P99 only 137ms vs 691ms. H2 1KB also leads by 48%, P95–P99 spread just 374–394ms vs 669–853ms. Root cause: tokio async runtime scheduling overhead is far lower than Nginx's epoll + worker process model for massive short-lived requests.

**2. HTTP/3 dominance across all file sizes**

H3 leads by 115%–271% from 1KB to 10MB, with the gap widening as file size increases: 10MB Sweety 20.3 RPS / 216 MB/s vs Nginx 5.47 RPS / 82 MB/s (**+271%**). 1MB P99 only 1.95s vs Nginx 13.94s, memory only 204 MB vs 672 MB. Sweety's quinn/h3 QUIC implementation is far more efficient in UDP multiplexing, congestion control (BBR), and backpressure than Nginx's QUIC implementation. v0.2.0 introduces connection-level memory limiting (`max_handlers`), completely resolving OOM under high-concurrency large file transfers.

**3. Memory efficiency**

Idle footprint 8.65 MB vs 75.34 MB (**−88%**). Under load, 44–79% less memory in most scenarios. H3 1MB: 204 MB vs 672 MB (−70%), H2 1MB: 428 MB vs 615 MB (−30%).

**4. Tail latency control**

H1/H2 small-file scenarios show extremely tight P95–P99 spread with far lower stdev than Nginx. H3 1MB P99 only 1.95s (Nginx 13.94s), thanks to connection-level backpressure and global pread_stream semaphore dual control.

**5. Zero errors**

All test scenarios: zero request failures, zero timeouts. Nginx stalled 72% of connections in H2 100KB×1000 (only 3,864 requests completed vs Sweety's 34,797).

**6. Protocol coverage**

Single process serves H1 + H2 + H3 simultaneously, no extra compile-time modules needed. Nginx HTTP/3 requires recompilation and performs significantly worse.

### Nginx Advantages

**1. Ecosystem & production validation**

20 years of production deployment, extensive documentation, mature third-party module ecosystem (WAF, Lua, OpenResty, etc.), global-scale operational experience and tooling. Vast community resources and professional support channels available when issues arise.

**2. L4 proxy**

Nginx `stream {}` module supports TCP/UDP L4 proxying for databases, SSH, and arbitrary TCP protocols. Sweety has not yet implemented this feature.

### Sweety Immaturity

| Area | Status | Details |
|------|--------|---------|
| **Production validation** | ⚠️ Not production-tested | No long-term high-traffic real-world deployment; reliability, edge cases, memory leaks not fully validated |
| **Module ecosystem** | Basic plugin system | Only Rust trait registration; no Lua/WAF/OpenResty mature ecosystem |
| **L4 proxy** | ❌ Not implemented | No `stream {}`-style TCP/UDP passthrough |
| **Conditional logic** | ❌ No `if` / `map` | No config-level conditional branching or variable mapping |
| **Community size** | Very small | Documentation, tutorials, third-party integrations all in early stages |
| **Long-term stability** | Unknown | No months-long sustained high-load data; GC-free but Rust unsafe boundaries require ongoing audit |

### Use Case Comparison

| Scenario | Recommended | Rationale |
|----------|-------------|-----------|
| **API gateway / microservice ingress** | **Sweety** | 5–6× RPS, P99 < 120ms, memory only ~50 MB |
| **HTTP/3 deployment** | **Sweety** | 2–4× RPS across all sizes, Nginx QUIC clearly inferior |
| **Edge nodes / embedded** | **Sweety** | 8.65 MB idle, single binary no deps, ideal for constrained environments |
| **Small static sites** | **Sweety** | One-line preset config, ACME auto-cert, works out of the box |
| **Large CDN / file delivery** | **Nginx** | BW-ceiling parity, more mature ops ecosystem |
| **WAF / Lua extensions needed** | **Nginx** | OpenResty / ModSecurity mature security ecosystem |
| **TCP/UDP L4 proxy** | **Nginx** | Sweety does not yet support stream module |
| **Critical production workloads** | **Nginx** | 20 years of production validation; Sweety is not yet production-proven |

---

## Test Environment Configuration

### Sweety Configuration

See [config/sweety.config.example](https://github.com/ChuYao233/Sweety/blob/main/config/sweety.config.example)

```toml
# ═══════════════════════════════════════════════════════════════════
# Global Configuration
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
# Site Configuration
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

### Nginx Configuration

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

## Reproducing These Results

1. Prepare test files:
```bash
dd if=/dev/urandom of=/www/wwwroot/local/1kb.bin bs=1K count=1
dd if=/dev/urandom of=/www/wwwroot/local/10kb.bin bs=10K count=1
dd if=/dev/urandom of=/www/wwwroot/local/100kb.bin bs=100K count=1
dd if=/dev/urandom of=/www/wwwroot/local/1mb.bin bs=1M count=1
dd if=/dev/urandom of=/www/wwwroot/local/10mb.bin bs=10M count=1
```

2. Run benchmarks:
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

3. Monitor resources during each test:
```bash
# In a separate terminal
watch -n 0.5 'ps -o pid,rss,%cpu,comm -p $(pgrep sweety)'
```
