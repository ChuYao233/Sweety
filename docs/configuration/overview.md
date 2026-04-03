# 配置文件概述

## 格式

Sweety 支持三种配置格式，通过文件扩展名自动识别：

| 扩展名 | 格式 |
|--------|------|
| `.toml` | TOML（推荐） |
| `.json` | JSON |
| `.yaml` / `.yml` | YAML |

## 结构

```toml
# 全局配置（可选，有合理默认值）
[global]
worker_threads = 0
log_level      = "info"
# ...

# 站点列表（一个或多个）
[[sites]]
name        = "site1"
server_name = ["example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/site1"

  # 站点级 TLS 配置
  [sites.tls]
  acme       = true
  acme_email = "your@email.com"

  # 站点级 FastCGI 配置
  [sites.fastcgi]
  socket = "/run/php/php8.2-fpm.sock"

  # 路由规则（可多个）
  [[sites.locations]]
  path    = "/"
  handler = "php"

  # 上游服务器组（反向代理用）
  [[sites.upstreams]]
  name  = "backend"
  nodes = [{ addr = "127.0.0.1:3000" }]
```

## 最简配置

### 纯静态站点（HTTP）

```toml
[[sites]]
name        = "static"
server_name = ["example.com"]
listen      = [80]
root        = "/var/www/html"
```

### 自动 HTTPS 静态站点

```toml
[[sites]]
name        = "static"
server_name = ["example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/html"
acme_email  = "your@email.com"
```

### WordPress（开箱即用）

```toml
[[sites]]
name        = "wp"
server_name = ["example.com"]
listen      = [80]
listen_tls  = [443]
root        = "/var/www/wordpress"
preset      = "wordpress"
php_fastcgi = "/run/php/php8.2-fpm.sock"
acme_email  = "your@email.com"
```

## 配置加载流程

```
读取文件
   ↓
TOML/JSON/YAML 解析
   ↓
expand_config()        ← 展开语法糖字段（preset、php_fastcgi、acme_email）
   ↓
validate_config()      ← 验证必填项、证书路径、端口冲突
   ↓
启动服务
```

## 配置文件路径

默认路径：`config/sweety.toml`

通过环境变量覆盖：

```bash
SWEETY_CONFIG=/etc/sweety/sweety.toml sweety run
```

通过命令行参数覆盖：

```bash
sweety run -c /etc/sweety/sweety.toml
```
