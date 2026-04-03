# 编译与安装

## 系统要求

| 依赖 | 最低版本 | 说明 |
|------|----------|------|
| Rust | 1.75+ | 推荐使用 `rustup` 管理 |
| OpenSSL / aws-lc-rs | — | TLS 依赖，自动编译 |
| CMake | 3.12+ | aws-lc-rs 编译依赖（Linux/macOS） |

> Windows 需额外安装 [NASM](https://www.nasm.us/) 和 Visual C++ Build Tools。

## 从源码编译

```bash
git clone https://github.com/ChuYao233/Sweety.git
cd Sweety

# 生产构建（开启全部优化）
cargo build --release

# 二进制位于
./target/release/sweety
```

### 功能特性开关（Cargo features）

| Feature | 默认 | 说明 |
|---------|------|------|
| `jemalloc` | 关 | 使用 jemalloc 替换系统分配器（Linux/macOS），高并发场景可提升 ~10% 性能 |

```bash
# 启用 jemalloc
cargo build --release --features jemalloc
```

## 安装到系统

```bash
# 复制到 /usr/local/bin
sudo cp target/release/sweety /usr/local/bin/
sweety --version
```

## systemd 服务

将以下内容保存为 `/etc/systemd/system/sweety.service`：

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

## 目录布局建议

```
/etc/sweety/
├── sweety.toml          # 主配置
├── certs/               # 手动证书（或 ACME 自动管理）
│   ├── example.com.crt
│   └── example.com.key
└── acme/                # ACME 自动证书缓存目录
```
