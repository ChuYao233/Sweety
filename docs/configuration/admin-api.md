# 管理 API

Sweety 内置了功能完整的 REST 管理 API，对标 Caddy Admin API 全部能力并扩展更多端点。
API 作为独立 TCP 监听器运行，不影响主服务器性能。

## 启用

```toml
[global]
admin_listen = "127.0.0.1:9099"   # 监听地址（空 = 禁用）
admin_token  = "your-secret-token" # Bearer Token（空 = 不鉴权）
```

> ⚠️ **安全建议**：只监听 `127.0.0.1`，**不要暴露到公网**。

## 鉴权

设置 `admin_token` 后，除以下路径外所有请求需携带 `Authorization: Bearer <token>` 头：

- `GET /api/health`
- `GET /health`
- `GET /api/version`
- `GET /api/doc`
- `GET /metrics`

## 核心概念

### 运行时配置 vs 磁盘配置

- **所有修改默认只影响运行时内存**，不写入磁盘。重启后恢复磁盘上的配置
- `GET /config` 始终返回**当前运行中**的配置，而非磁盘文件内容
- 追加 `?save=true` 参数可同时将修改持久化到配置文件（TOML 格式）
- `POST /config/save` 可随时显式保存当前运行配置到磁盘

```bash
# 仅修改运行时（重启后恢复）
curl -X PATCH http://127.0.0.1:9099/config/global \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"keepalive_timeout": 120}'

# 修改运行时 + 持久化到配置文件
curl -X PATCH "http://127.0.0.1:9099/config/global?save=true" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"keepalive_timeout": 120}'

# 显式保存当前运行配置到磁盘
curl -X POST http://127.0.0.1:9099/config/save \
  -H "Authorization: Bearer $TOKEN"
```

---

## 端点一览

### 配置树 CRUD（对标 Caddy `/config/`）

| 方法 | 路径 | 说明 |
|------|------|------|
| `POST` | `/load` | 整体热加载 JSON 配置（失败自动回滚） |
| `GET` | `/config/[path]` | 读取运行中配置子树 |
| `POST` | `/config/[path]` | 创建/替换对象 \| 追加数组元素 |
| `PUT` | `/config/[path]` | 数组按索引插入 \| 严格创建（已有报错） |
| `PATCH` | `/config/[path]` | 仅替换已有值 |
| `DELETE` | `/config/[path]` | 删除节点（`/config/` = 清空配置不退出） |
| `POST` | `/config/save` | 显式保存运行配置到磁盘（TOML） |
| `POST` | `/config/reload` | 从磁盘热重载配置 |
| `POST` | `/config/test` | 验证磁盘配置文件语法 |

所有写操作支持 `?save=true` 查询参数，同时持久化到配置文件。

### @id 节点直达（对标 Caddy `/id/`）

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/id/:id` | 通过 `@id` 直接访问配置节点 |
| `GET` | `/id/:id/[path]` | 通过 `@id` + 子路径访问 |

### 配置适配器（对标 Caddy `/adapt`）

| 方法 | 路径 | 说明 |
|------|------|------|
| `POST` | `/adapt` | TOML → JSON 配置转换（不加载，仅转换） |

### 运行时状态（对标 Caddy）

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/reverse_proxy/upstreams` | 上游节点状态（Caddy 兼容 JSON 格式） |
| `GET` | `/metrics` | Prometheus text/plain 指标 |
| `POST` | `/api/stop` | 优雅停机 |

### 系统管理

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/health` | 健康检查（无需鉴权） |
| `GET` | `/api/version` | 版本 + 构建信息（无需鉴权） |
| `GET` | `/api/system` | 系统信息（uptime / workers / memory） |
| `GET` | `/api/doc` | API 文档 JSON（无需鉴权） |
| `GET` | `/api/debug` | 运行时调试信息 |
| `GET` | `/api/stats` | 全局统计快照 |

### 站点管理

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/sites` | 站点列表 + 摘要 |
| `GET` | `/api/sites/:name` | 单个站点详情 |
| `DELETE` | `/api/sites/:name` | 删除站点（热生效） |

### 上游管理

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/upstreams` | 所有上游组 + 节点状态 |
| `GET` | `/api/upstreams/:name` | 单个上游组详情 |
| `POST` | `/api/upstreams/:name/nodes/:addr/enable` | 启用节点 |
| `POST` | `/api/upstreams/:name/nodes/:addr/disable` | 禁用节点 |
| `PUT` | `/api/upstreams/:name/nodes/:addr/weight` | 修改节点权重 |

### 证书管理

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/certs` | TLS 证书列表 |
| `POST` | `/api/certs/reload` | 重新加载磁盘证书 |
| `POST` | `/api/certs/acme/renew` | 立即触发 ACME 证书续期（`?site=name` 指定站点） |

### 缓存管理

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/cache/stats` | 缓存命中率统计 |
| `POST` | `/api/cache/purge` | 清除所有缓存 |

### 连接 / 插件 / 日志

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/api/connections` | 活跃连接数 + 连接池状态 |
| `GET` | `/api/plugins` | 已注册插件列表 |
| `GET` | `/api/logs/level` | 当前日志级别 |
| `PUT` | `/api/logs/level` | 修改日志级别 |

---

## 使用示例

### 健康检查

```bash
curl http://127.0.0.1:9099/api/health
# {"status":"ok"}
```

### 查看运行中配置

```bash
# 完整配置
curl http://127.0.0.1:9099/config/ -H "Authorization: Bearer $TOKEN"

# 仅全局配置
curl http://127.0.0.1:9099/config/global -H "Authorization: Bearer $TOKEN"

# 某个站点（按数组索引）
curl http://127.0.0.1:9099/config/sites/0 -H "Authorization: Bearer $TOKEN"
```

### 修改全局配置

```bash
# 修改 keepalive_timeout（仅运行时）
curl -X PATCH http://127.0.0.1:9099/config/global \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"keepalive_timeout": 120}'

# 修改并保存到磁盘
curl -X PATCH "http://127.0.0.1:9099/config/global?save=true" \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"keepalive_timeout": 120}'
```

### 整体热加载配置

```bash
curl -X POST http://127.0.0.1:9099/load \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d @new-config.json
```

失败时自动回滚到上一份配置。

### 追加站点

```bash
curl -X POST http://127.0.0.1:9099/config/sites \
  -H "Authorization: Bearer $TOKEN" \
  -d '{
    "name": "new-site",
    "server_name": ["new.example.com"],
    "listen": [80],
    "root": "/var/www/new-site",
    "locations": [{"path": "/", "handler": "static"}]
  }'
```

### 删除站点

```bash
# 通过配置树路径删除第 2 个站点
curl -X DELETE http://127.0.0.1:9099/config/sites/1 \
  -H "Authorization: Bearer $TOKEN"

# 或通过站点名删除
curl -X DELETE http://127.0.0.1:9099/api/sites/my-site \
  -H "Authorization: Bearer $TOKEN"
```

### 上游节点控制

```bash
# 禁用节点（摘流）
curl -X POST http://127.0.0.1:9099/api/upstreams/backend/nodes/127.0.0.1%3A8080/disable \
  -H "Authorization: Bearer $TOKEN"

# 启用节点
curl -X POST http://127.0.0.1:9099/api/upstreams/backend/nodes/127.0.0.1%3A8080/enable \
  -H "Authorization: Bearer $TOKEN"

# 修改权重
curl -X PUT http://127.0.0.1:9099/api/upstreams/backend/nodes/127.0.0.1%3A8080/weight \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"weight": 5}'
```

### TOML → JSON 适配

```bash
curl -X POST http://127.0.0.1:9099/adapt \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: text/plain" \
  --data-binary @sweety.toml
```

### Prometheus 指标

```bash
curl http://127.0.0.1:9099/metrics
```

可用指标：

| 指标名 | 类型 | 说明 |
|--------|------|------|
| `sweety_requests_total` | counter | 累计处理请求总数 |
| `sweety_active_requests` | gauge | 当前并发请求数 |
| `sweety_errors_4xx_total` | counter | 累计 4xx 错误数 |
| `sweety_errors_5xx_total` | counter | 累计 5xx 错误数 |
| `sweety_bytes_sent_total` | counter | 累计响应字节数 |
| `sweety_websocket_connections` | gauge | 当前活跃 WebSocket 连接数 |

启用分析报告时还包含：`sweety_avg_response_ms`、`sweety_error_rate_5xx`、`sweety_status_total{code="..."}` 等。

### 统计快照（/api/stats）

```bash
curl http://127.0.0.1:9099/api/stats -H "Authorization: Bearer $TOKEN"
```

返回 JSON：

```json
{
  "total_requests": 12345,
  "total_errors_4xx": 23,
  "total_errors_5xx": 2,
  "total_bytes_sent": 1048576,
  "active_requests": 5,
  "active_ws_connections": 1
}
```

### 修改日志级别

```bash
curl -X PUT http://127.0.0.1:9099/api/logs/level \
  -H "Authorization: Bearer $TOKEN" \
  -d '{"level": "debug"}'
```

### 清除缓存

```bash
curl -X POST http://127.0.0.1:9099/api/cache/purge \
  -H "Authorization: Bearer $TOKEN"
```

### ACME 证书即时续期

```bash
# 续期所有 ACME 站点的证书
curl -X POST http://127.0.0.1:9099/api/certs/acme/renew \
  -H "Authorization: Bearer $TOKEN"

# 仅续期指定站点（按站点名）
curl -X POST "http://127.0.0.1:9099/api/certs/acme/renew?site=my-api" \
  -H "Authorization: Bearer $TOKEN"
# 返回 202：续期在后台异步执行，失败时继续使用当前证书
# 一张 SAN 证书覆盖站点所有域名（example.com + www.example.com）
```

### 查看 API 文档

```bash
# 通过 HTTP
curl http://127.0.0.1:9099/api/doc | jq .

# 通过命令行
sweety --api-doc
```

---

## 与 Caddy Admin API 对比

| 功能 | Caddy | Sweety |
|------|-------|--------|
| 配置树 CRUD `/config/[path]` | ✅ | ✅ |
| 整体热加载 `/load` | ✅ | ✅ + 失败自动回滚 |
| 清空配置不退出 `DELETE /config/` | ✅ | ✅ |
| `@id` 节点直达 `/id/:id` | ✅ | ✅ |
| 配置适配器 `/adapt` | ✅ Caddyfile→JSON | ✅ TOML→JSON |
| 上游状态 `/reverse_proxy/upstreams` | ✅ | ✅ |
| Prometheus `/metrics` | ✅ | ✅ text/plain |
| 优雅停机 `/stop` | ✅ | ✅ |
| 运行时持久化控制 `?save=true` | ✗（总是持久化） | ✅ 可选 |
| 显式保存 `/config/save` | ✗ | ✅ |
| 从磁盘重载 `/config/reload` | ✗ | ✅ |
| 配置语法验证 `/config/test` | ✗ | ✅ |
| 站点 CRUD `/api/sites` | ✗ | ✅ |
| 上游节点控制 enable/disable/weight | ✗ | ✅ |
| 证书管理 `/api/certs` | ✗ | ✅ |
| 缓存管理 `/api/cache` | ✗ | ✅ |
| 运行时调试 `/api/debug` | `/debug/pprof` | ✅ |
| 日志级别热切换 `/api/logs/level` | ✗ | ✅ |
| 插件列表 `/api/plugins` | ✗ | ✅ |
| Bearer Token 鉴权 | Mutual TLS | ✅ |
| ACME 即时续期 `/api/certs/acme/renew` | ✗ | ✅ 异步 + SAN 多域名 |
| CORS 支持 | ✗ | ✅ |
| API 文档端点 `/api/doc` | ✗ | ✅ |

---

## CLI 集成

```bash
# 输出 API 文档 JSON
sweety --api-doc

# 触发热重载（等价 POST /config/reload）
sweety reload

# 验证配置文件
sweety validate -c sweety.toml
```
