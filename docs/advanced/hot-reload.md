# 热重载

热重载允许在**不断开现有连接**的情况下重新加载配置文件，等价 `nginx -s reload`。

## 前提条件

必须配置 Admin API 监听地址：

```toml
[global]
admin_listen = "127.0.0.1:9099"
admin_token  = "your-secret-token"   # 可选，建议生产环境配置
```

## 触发热重载

```bash
sweety reload
sweety reload -c /etc/sweety/sweety.toml
```

等价 API 调用：

```bash
curl -X POST \
  -H "Authorization: Bearer your-secret-token" \
  http://127.0.0.1:9099/api/reload
```

## 热重载范围

| 配置项 | 热重载支持 |
|--------|-----------|
| 站点 `server_name` / `root` / `index` | ✅ |
| `[[sites.locations]]` 路由规则 | ✅ |
| `[[sites.upstreams]]` 上游节点 | ✅ |
| `[sites.fastcgi]` FastCGI 配置 | ✅ |
| `[sites.rate_limit]` 限流规则 | ✅ |
| `[sites.proxy_cache]` 缓存配置 | ✅ |
| `[global]` 日志级别 | ✅ |
| 监听端口（`listen` / `listen_tls`） | ⚠️ 需重启 |
| TLS 证书文件路径 | ⚠️ 需重启 |
| `[global] worker_threads` | ⚠️ 需重启 |

## systemd 集成

在 systemd 单元文件中配置 `ExecReload`，使 `systemctl reload sweety` 触发热重载：

```ini
[Service]
ExecStart  = /usr/local/bin/sweety run -c /etc/sweety/sweety.toml
ExecReload = /usr/local/bin/sweety reload -c /etc/sweety/sweety.toml
```

```bash
# 重载配置（不中断连接）
sudo systemctl reload sweety

# 完整重启（断开所有连接）
sudo systemctl restart sweety
```

## 配置变更工作流

```bash
# 1. 编辑配置
vim /etc/sweety/sweety.toml

# 2. 校验语法
sweety validate -c /etc/sweety/sweety.toml

# 3. 热重载（零停机）
sweety reload -c /etc/sweety/sweety.toml

# 或一步完成
sweety validate -c /etc/sweety/sweety.toml && sweety reload -c /etc/sweety/sweety.toml
```

## 监控重载结果

重载完成后可通过 API 确认配置已更新：

```bash
curl -H "Authorization: Bearer your-secret-token" \
  http://127.0.0.1:9099/api/status
```

日志中会输出：

```
INFO sweety::config::hot_reload: 配置热重载完成，站点数: 3
```
