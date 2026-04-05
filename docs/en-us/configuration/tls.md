# TLS / HTTPS / ACME

## Simplest HTTPS (ACME Auto-Certificate)

```toml
[[sites]]
name        = "my-site"
server_name = ["example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/html"
```

Just configure `listen_tls` and Sweety automatically enables ACME (Caddy-style). `acme_email` is optional — a random email is generated if omitted.

Equivalent full syntax:

```toml
[sites.tls]
acme       = true
# acme_email = "your@email.com"   # Optional, for expiry notifications
```

## Full TLS Configuration

```toml
[sites.tls]
# ─── Certificate Source (choose one) ─────────────────────────────
# Method 1: ACME auto-certificate
acme             = true
acme_email       = "your@email.com"   # Optional, random email if omitted
acme_provider    = "letsencrypt"   # letsencrypt / zerossl / litessl / custom URL
acme_challenge   = "http01"        # http01 / dns01
acme_renew_days_before = 30        # Auto-renew N days before expiry
# ZeroSSL / LiteSSL EAB credentials are fetched automatically

# Method 2: Manual single certificate
cert = "/etc/ssl/example.com.crt"
key  = "/etc/ssl/example.com.key"

# Method 3: Multi-certificate (SNI routing, different certs per domain on same port)
[[sites.tls.certs]]
cert = "/etc/ssl/example.com.crt"
key  = "/etc/ssl/example.com.key"

[[sites.tls.certs]]
cert = "/etc/ssl/example.org.crt"
key  = "/etc/ssl/example.org.key"

# ─── TLS Version Control ─────────────────────────────────────────
min_version = "tls1.2"   # tls1.2 / tls1.3 (default tls1.2)
max_version = "tls1.3"   # default tls1.3

# ─── Protocol List (ALPN, affects HTTP/2 and HTTP/3 negotiation) ─
protocols = ["h3", "h2", "http/1.1"]   # Default all enabled, order = priority

# ─── HTTP/3 QUIC Tuning ──────────────────────────────────────────
[sites.tls.http3]
max_concurrent_bidi_streams = 200
max_concurrent_uni_streams  = 100
idle_timeout_ms              = 30000
keep_alive_interval_ms       = 10000
receive_window               = 8388608   # 8MB
stream_receive_window        = 2097152   # 2MB
send_window                  = 8388608   # 8MB
enable_0rtt                  = false
mtu_discovery                = true
initial_rtt_ms               = 333
max_ack_delay_ms             = 25
```

## ACME Multi-Domain SAN Certificates

When a site has multiple `server_name` entries, ACME automatically issues a single **SAN certificate** covering all domains — no extra configuration needed:

```toml
[[sites]]
name        = "my-site"
server_name = ["example.com", "www.example.com", "api.example.com"]
listen      = [80]
listen_tls  = [443]

[sites.tls]
acme = true
# acme_email = "admin@example.com"   # Optional
# → Automatically issues one SAN certificate for all 3 domains
```

- Certificate file is named after the first non-wildcard domain (e.g. `example.com.crt`)
- Auto-renewal check runs every 12 hours (renews 30 days before expiry)
- Renewal failure does not affect the current certificate — only logged

## ACME Instant Renewal

Trigger certificate renewal immediately via the Admin API (without waiting for the auto-check cycle):

```bash
# Renew all ACME sites
curl -X POST http://127.0.0.1:9099/api/certs/acme/renew \
  -H "Authorization: Bearer $TOKEN"

# Renew a specific site only
curl -X POST "http://127.0.0.1:9099/api/certs/acme/renew?site=my-site" \
  -H "Authorization: Bearer $TOKEN"
```

Returns 202 Accepted — renewal runs asynchronously in the background. See [Admin API](./admin-api.md) for details.

## ACME DNS-01 Validation (Wildcard Certificates)

DNS-01 validation can issue `*.example.com` wildcard certificates:

```toml
[sites.tls]
acme           = true
# acme_email   = "your@email.com"   # Optional
acme_challenge = "dns01"

# Cloudflare DNS
[sites.tls.dns_provider]
type      = "cloudflare"
api_token = "your-cloudflare-api-token"
zone_id   = "optional-zone-id"   # Auto-detected if omitted

# Aliyun DNS
# [sites.tls.dns_provider]
# type              = "aliyun"
# access_key_id     = "your-key-id"
# access_key_secret = "your-key-secret"

# Custom Shell Script
# [sites.tls.dns_provider]
# type       = "shell"
# set_script = "/etc/sweety/dns-set.sh"
# del_script = "/etc/sweety/dns-del.sh"  # Optional
```

## Protocol Control

The `protocols` field controls which HTTP versions are supported, used in ALPN negotiation:

```toml
# HTTP/1.1 only (disable H2/H3)
protocols = ["http/1.1"]

# HTTP/2 only
protocols = ["h2"]

# HTTP/3 only (not recommended, browsers can't discover on first visit)
protocols = ["h3"]

# Default (all supported)
protocols = ["h3", "h2", "http/1.1"]
```

When multiple sites share the same TLS port, the ALPN protocol list is the **union** of all sites — if any site supports h3, that port enables UDP listening.

## HTTP/3 Firewall Notes

HTTP/3 uses UDP 443 — ensure your firewall allows it:

```bash
# iptables
iptables -A INPUT -p udp --dport 443 -j ACCEPT

# firewalld
firewall-cmd --add-port=443/udp --permanent
firewall-cmd --reload
```
