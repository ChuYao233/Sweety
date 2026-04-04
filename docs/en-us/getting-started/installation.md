# Installation

## System Requirements

| Dependency | Minimum Version | Notes |
|------------|-----------------|-------|
| Rust | 1.75+ | Recommended to manage via `rustup` |
| OpenSSL / aws-lc-rs | — | TLS dependency, auto-compiled |
| CMake | 3.12+ | Required for aws-lc-rs (Linux/macOS) |

> Windows requires [NASM](https://www.nasm.us/) and Visual C++ Build Tools.

## Build from Source

```bash
git clone https://github.com/ChuYao233/Sweety.git
cd Sweety

# Production build (all optimizations enabled)
cargo build --release

# Binary located at
./target/release/sweety
```

### Cargo Features

| Feature | Default | Description |
|---------|---------|-------------|
| `jemalloc` | Off | Use jemalloc instead of system allocator (Linux/macOS), ~10% improvement under high concurrency |

```bash
# Enable jemalloc
cargo build --release --features jemalloc
```

## Install to System

```bash
# Copy to /usr/local/bin
sudo cp target/release/sweety /usr/local/bin/
sweety --version
```

## systemd Service

Save the following as `/etc/systemd/system/sweety.service`:

```ini
[Unit]
Description=Sweety Web Server
After=network.target

[Service]
Type=simple
User=www-data
ExecStart=/usr/local/bin/sweety run -c /etc/sweety/sweety.toml
ExecReload=/usr/local/bin/sweety reload -c /etc/sweety/sweety.toml
Restart=on-failure
LimitNOFILE=65535

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now sweety
```

## Recommended Directory Layout

```
/etc/sweety/
├── sweety.toml          # Main config
├── certs/               # Manual certificates (or ACME auto-managed)
│   ├── example.com.crt
│   └── example.com.key
└── acme/                # ACME certificate cache directory
```
