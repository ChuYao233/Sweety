# HTTP/3 Configuration & Tuning

HTTP/3 is based on QUIC (UDP) and shares port 443 with HTTP/2. Clients first connect via HTTP/2, then the server advertises HTTP/3 support via the `Alt-Svc` header, and subsequent requests upgrade.

## Enabling HTTP/3

HTTP/3 is **automatically enabled with HTTPS** — no extra configuration needed:

```toml
[[sites]]
listen_tls = [443]
acme_email = "your@email.com"
# protocols defaults to ["h3", "h2", "http/1.1"], h3 auto-enabled
```

## Disabling HTTP/3

```toml
[sites.tls]
protocols = ["h2", "http/1.1"]   # Exclude h3
```

## Full HTTP/3 Tuning Configuration

```toml
[sites.tls.http3]
# ─── Connection-Level Rate Limiting ──────────────────────────────
max_handlers = 0                     # Max concurrent QUIC connections (0 = auto-detect, supports hot-reload)

# ─── Concurrency Control ─────────────────────────────────────────
max_concurrent_bidi_streams = 200    # Max concurrent bidirectional streams per connection (default 200)
max_concurrent_uni_streams  = 100    # Max concurrent unidirectional streams per connection (default 100)

# ─── Timeouts ────────────────────────────────────────────────────
idle_timeout_ms          = 30000     # Idle connection timeout (ms, default 30s)
keep_alive_interval_ms   = 10000     # Keep-Alive PING interval (ms, default 10s)

# ─── Flow Control Windows ────────────────────────────────────────
receive_window        = 8388608    # Connection-level receive window (bytes, default 8MB)
stream_receive_window = 2097152    # Stream-level receive window (bytes, default 2MB)
send_window           = 8388608    # Connection-level send window (bytes, default 8MB)

# ─── Connection Optimization ─────────────────────────────────────
enable_0rtt      = false   # 0-RTT Early Data (default off, replay attack risk)
mtu_discovery    = true    # PMTU discovery (default on, optimizes large packet transfer)
initial_rtt_ms   = 333     # Initial RTT estimate (ms, quinn default)
max_ack_delay_ms = 25      # Max ACK delay (ms, RFC 9000 default)
```

## Connection-Level Memory Limiting

`max_handlers` controls the global max concurrent QUIC connections, preventing OOM during high-concurrency large file transfers:

```toml
[sites.tls.http3]
# Max concurrent QUIC connections (0 = auto-detect)
# Auto formula: available_memory * 80% / 16MB / num_workers
# Each QUIC connection buffers up to send_window bytes; this limit prevents OOM
# Excess connections are queued, not rejected
# Supports hot-reload: takes effect immediately after Admin API config reload
max_handlers = 0
```

| Available Memory | Auto Value (2 cores) | Auto Value (4 cores) |
|-----------------|---------------------|---------------------|
| 512MB           | ~12                 | ~6                  |
| 1GB             | ~25                 | ~12                 |
| 2GB             | ~51                 | ~25                 |
| 8GB             | ~204                | ~102                |

> **Nginx has no equivalent configuration** and will also OOM in the same scenario. Sweety's `max_handlers` provides finer-grained H3 memory control than `worker_connections`.
>
> For benchmarking, set a higher value manually (e.g. `max_handlers = 200`) to avoid overly conservative auto-calculation.

## Tuning Recommendations

### High Concurrency

```toml
[sites.tls.http3]
max_concurrent_bidi_streams = 500
receive_window        = 16777216   # 16MB
stream_receive_window = 4194304    # 4MB
send_window           = 16777216   # 16MB
```

### High Latency Networks (Cross-country / Mobile)

```toml
[sites.tls.http3]
idle_timeout_ms        = 60000   # Extend idle timeout
keep_alive_interval_ms = 15000   # Extend Keep-Alive interval
initial_rtt_ms         = 100     # Manually set lower initial RTT (when latency is known)
```

### Enable 0-RTT (Faster First Request)

> ⚠️ 0-RTT has replay attack risks, only safe for idempotent (GET/HEAD) requests

```toml
[sites.tls.http3]
enable_0rtt = true
```

## Firewall Configuration

HTTP/3 uses **UDP 443**, which must be allowed:

```bash
# iptables
iptables -A INPUT -p udp --dport 443 -j ACCEPT

# nftables
nft add rule inet filter input udp dport 443 accept

# firewalld
firewall-cmd --add-port=443/udp --permanent && firewall-cmd --reload

# ufw
ufw allow 443/udp
```

## Verify HTTP/3

```bash
# Using curl (requires curl >= 7.88 with HTTP/3 support)
curl -I --http3 https://your.domain.com

# Response headers should include
# alt-svc: h3=":443"; ma=86400

# Check actual protocol used
curl -I --http3 -w "%{http_version}\n" https://your.domain.com
```

Browser verification: Open Chrome DevTools → Network → Protocol column, should show `h3`.
