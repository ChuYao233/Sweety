# Sweety

[![GitHub release](https://img.shields.io/github/v/tag/ChuYao233/Sweety)](https://github.com/ChuYao233/Sweety/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/ChuYao233/Sweety/blob/main/LICENSE)
[![GitHub last commit](https://img.shields.io/github/last-commit/ChuYao233/Sweety)](https://github.com/ChuYao233/Sweety/commits/main)
[![GitHub issues](https://img.shields.io/github/issues/ChuYao233/Sweety)](https://github.com/ChuYao233/Sweety/issues)

[简体中文](/README_CN.md) | [English](/README.md)

> A high-performance, single-binary, multi-site web server written in pure Rust.
> Nginx-level tunability meets Caddy-style simplicity.

The underlying HTTP stack is forked from [xitca-web](https://github.com/HFQR/xitca-web) and independently maintained under `vendor/`, with numerous production-oriented performance fixes and optimizations.

📚 **[Documentation](https://sweety.2o.nz)** | ⚙️ **[Config Example](config/sweety.config.example)** | � **[Benchmarks](https://sweety.2o.nz/benchmark/)** | �️ **[Roadmap](https://sweety.2o.nz/roadmap/)**

---

## Features

### Protocol Support

- 🌐 **HTTP/1.1 + HTTP/2 + HTTP/3 (QUIC)** — all protocols served from a single process
- 🔒 **TLS** — pure Rust via Rustls, zero OpenSSL dependency; multi-cert SNI with automatic selection
- 📜 **ACME Auto-Certificates** — HTTP-01 + DNS-01, supports Let's Encrypt / ZeroSSL / LiteSSL; wildcard certs; self-signed placeholder on startup, hot-reload on issuance (Caddy-style)
- 🔌 **WebSocket** — H1 Upgrade (RFC 6455) + H2 extended CONNECT (RFC 8441) full passthrough

### Request Handling

- 📁 **Static Files** — in-memory cache + Range + ETag/Last-Modified + `try_files` + pread streaming
- 🐘 **PHP / FastCGI** — Unix socket / TCP connection pool + `fastcgi_cache`; correct HTTP/2 Cookie merging (RFC 7540 §8.1.2.5), compatible with WordPress / Laravel
- 🔄 **Reverse Proxy** — round-robin / weighted / least-conn / IP hash + connection pool + active health checks + `proxy_cache`
- 📡 **gRPC Proxy** — automatic `application/grpc` + Trailer handling
- 🔑 **auth_request** — subrequest-based authentication

### Routing

- 🏠 **Virtual Hosts** — exact match / wildcard / fallback catch-all
- 📍 **Location Priority** — `= exact` > `^~ prefix-priority` > `~ regex` > `prefix`
- ✏️ **Rewrite Rules** — regex capture, `last/break/redirect/permanent`, `!-f/!-d` conditions

### Performance Architecture

- ⚡ **SO_REUSEPORT Multi-Core Scaling** — each worker thread independently binds the same port, kernel load-balances, zero lock contention
- 🚀 **H2 Per-Connection Writer Loop** — dedicated writer task per connection, HEADERS-priority + round-robin DATA scheduling, eliminates head-of-line blocking
- ⚖️ **Write Fairness** — fixed 16KB chunk round-robin, prevents large downloads from starving small requests
- 💤 **Zero CPU Idle Spin** — writer loop is `tokio::select!` event-driven, no busy spin

### Reliability

- 🛡️ **Circuit Breaker** — three-state machine (Closed → Open → Half-Open), more precise than Nginx `max_fails`
- 🚦 **5-Dimension Rate Limiting** — IP / path / IP+path / header / User-Agent token buckets
- 🔥 **Hot Reload** — reload config without dropping existing connections (`nginx -s reload` equivalent)

### Operations

- 🖥️ **Admin REST API** — health / version / stats / plugins (`/api/v1/*`); site management, node control, WebSocket push planned for v0.5
- 📝 **Access Logs** — combined / JSON / custom template, async writer
- 📊 **Prometheus Metrics** — `/metrics` endpoint (planned for v0.5)

---

## Quick Start

### Installation

#### Build from Source

```bash
# Clone and build
git clone https://github.com/ChuYao233/Sweety.git
cd Sweety
cargo build --release

# The binary is at target/release/sweety
```

#### Download Pre-built Binary

Pre-built binaries for Linux (x86_64 musl static) are available on the [Releases](https://github.com/ChuYao233/Sweety/releases) page.

### Usage

```bash
# Validate configuration (equivalent to nginx -t)
./sweety validate

# Start the server
./sweety run

# Hot reload configuration
./sweety reload
```

### Minimal Configuration

```toml
[global]
log_level = "info"

[[sites]]
name        = "my-site"
server_name = ["example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/html"

# Automatic HTTPS — just add your email
acme_email = "you@example.com"

[[sites.locations]]
path    = "/"
handler = "static"
```

For a complete configuration reference with all options, see [config/sweety.config.example](config/sweety.config.example).

---

## Comparison

> ⚠️ Sweety has not yet been validated in production. Feedback from testing/staging environments is welcome.

| Feature | Sweety | Nginx | Caddy | Apache |
|---------|--------|-------|-------|--------|
| Built-in HTTP/3 | ✅ | ❌ Requires recompile | ✅ | ❌ Experimental |
| ACME Auto-Cert | ✅ HTTP-01 + DNS-01 | ❌ Needs certbot | ✅ | ❌ Needs plugin |
| Brotli Compression | ✅ Built-in | ❌ Third-party module | ✅ | ✅ mod_brotli |
| Circuit Breaker | ✅ 3-state FSM | ⚠️ max_fails only | ❌ | ❌ |
| WebSocket H2 (RFC 8441) | ✅ | ✅ | ✅ | ✅ |
| gRPC Proxy | ✅ | ✅ (full in Plus) | ✅ | ⚠️ Limited |
| FastCGI Response Cache | ✅ | ✅ | ❌ | ✅ |
| Static File Memory Cache | ✅ | ✅ OS page cache | ❌ | ✅ |
| Config Simplicity | ✅ Presets + sugar | ❌ Manual | ✅ Caddyfile | ⚠️ Verbose |
| Admin REST API | ⚠️ Partial (v0.5) | ❌ | ✅ | ❌ |
| Single Binary, No Deps | ✅ | ❌ | ✅ | ❌ |
| Memory Safety | ✅ Rust | ❌ C | ✅ Go | ❌ C |
| `if` / `map` Conditionals | ❌ | ✅ | ⚠️ Limited | ✅ mod_rewrite |
| TCP/UDP L4 Proxy | ❌ | ✅ stream | ❌ | ❌ |
| **Production Proven** | ⚠️ **Not yet** | ✅ Widely | ✅ Widely | ✅ Widely |

---

## Performance

> Test environment: 2C/1G Debian · TLSv1.3 · h2load 15s · 1000 connections

| Proto | File | Sweety RPS | Nginx RPS | Δ |
|-------|------|-----------|-----------|---|
| H1 | 1 KB | **106,695** | 18,480 | **+477%** |
| H2 | 1 KB | **27,276** | 18,479 | **+48%** |
| H3 | 1 KB | **33,104** | 15,411 | **+115%** |
| H3 | 10 KB | **14,638** | 5,564 | **+163%** |
| H2 | 100 KB | **2,320** | 258 | **+799%** |
| H3 | 1 MB | **209.7** | 68.5 | **+206%** |

- **P99 Latency**: H1 1KB 137ms vs 691ms (**−80%**); H3 1MB 1.95s vs 13.94s (**−86%**)
- **Memory**: Idle **8.65 MB** vs 75.34 MB (**−88%**); H3 1MB **204 MB** vs 672 MB (**−70%**)
- **Zero errors** across all test scenarios; Nginx H2 100KB×1000 had 72% connections stalled

👉 **[Full benchmark results and methodology](https://sweety.2o.nz/benchmark/)**

---

## Contributing

Contributions are welcome! Please open an issue or pull request on [GitHub](https://github.com/ChuYao233/Sweety).

## License

[Apache License 2.0](LICENSE)
