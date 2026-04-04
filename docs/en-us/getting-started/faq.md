# FAQ

## Startup Issues

### Port Already in Use

```
Error: Address already in use (os error 98)
```

Find the process occupying the port:

```bash
ss -tlnp | grep :443
# or
fuser 443/tcp
```

### Certificate Not Found

```
TLS error: no certificate found for example.com
```

- Check if the `root` path is correct
- If using ACME, confirm `acme_email` is set
- If using manual certificates, confirm `cert` / `key` paths exist and are readable
- Run `sweety validate` for detailed errors

### Permission Denied (Binding 80/443)

```bash
# Method 1: setcap (recommended, no root needed at runtime)
sudo setcap 'cap_net_bind_service=+ep' /usr/local/bin/sweety

# Method 2: systemd (recommended), add to [Service]
AmbientCapabilities=CAP_NET_BIND_SERVICE
```

---

## ACME / Certificate Issues

### Certificate Issuance Failed

1. Confirm domain DNS resolves to the server IP
2. Confirm port 80 is accessible (required for HTTP-01 validation)
3. Let's Encrypt has rate limits (5 per domain per week). Use staging for testing:

```toml
[sites.tls]
acme         = true
acme_email   = "your@email.com"
acme_provider = "https://acme-staging-v02.api.letsencrypt.org/directory"
```

### "Too Many Redirects" After HTTPS Redirect

Ensure `force_https = true` is only on the HTTP site config:

```toml
[[sites]]
listen     = [80]
listen_tls = [443]
force_https = true   # Only affects HTTP 80, HTTPS requests won't redirect again
```

---

## FastCGI / PHP Issues

### PHP Returns 502

1. Check if PHP-FPM is running: `systemctl status php8.2-fpm`
2. Check socket path: `ls -la /run/php/php8.2-fpm.sock`
3. Confirm Sweety's user has permission to access the socket

### PHP File Upload Fails

```toml
[global]
client_max_body_size = 100   # MB, default 50MB
```

Also confirm `upload_max_filesize` and `post_max_size` in php.ini are large enough.

---

## HTTP/3 Issues

### Browser Not Using HTTP/3

1. Confirm firewall allows UDP port 443
2. Confirm TLS certificate is valid (HTTP/3 does not accept self-signed certificates)
3. First visit uses HTTP/2; the browser discovers HTTP/3 via `Alt-Svc` header and upgrades on the next request

### Verify HTTP/3 is Working

```bash
curl -I --http3 https://your.domain.com
# Response headers should include alt-svc: h3=":443"
```

---

## Hot Reload Issues

### `sweety reload` Fails

Confirm `global.admin_listen` is configured:

```toml
[global]
admin_listen = "127.0.0.1:9099"
```

The reload command sends a signal via the Admin API — it won't work without this setting.

---

## Performance Issues

### 503 Under High Concurrency

Adjust:

```toml
[global]
worker_threads     = 0      # 0 = auto-detect CPU cores
worker_connections = 51200
max_connections    = 50000
```

System level:

```bash
# Increase file descriptor limit
ulimit -n 65535
# Or configure in /etc/security/limits.conf
```
