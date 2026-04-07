# 压缩

Sweety 原生支持 **Brotli（br）、zstd、gzip** 三种压缩算法，统一适用于静态文件、反向代理、PHP FastCGI 三种场景。默认全部开启，无需额外配置即可工作。

## 压缩行为概览

| 场景 | 压缩方式 | 触发条件 |
|------|----------|----------|
| **静态文件（≤ 1 MB）** | 预压缩 + 内存缓存 | 文件首次被请求时预生成三种编码，后续命中缓存零 CPU 开销 |
| **静态文件（> 1 MB）** | 流式压缩 | 按请求实时压缩，不占用内存缓存 |
| **反向代理** | 流式压缩 | 上游未压缩（无 `Content-Encoding`）且响应 mime 可压缩时压缩 |
| **PHP FastCGI** | 流式压缩 | PHP 未压缩输出时压缩，同上逻辑 |

**算法选择策略**：内容自适应，不固定优先级

| 情况 | 选择逻辑 |
|------|----------|
| 客户端有显式 `q=` 差异 | 尊重客户端偏好，选 q 最高的已开启算法 |
| 客户端 q 全部相等（无偏好）| 按响应大小自动选择最优算法 |

**响应大小自适应规则**（客户端无偏好时）：
- **≤ 20 KB** 或**流式未知大小** → `zstd`（解压速度极快，减少客户端 CPU 开销，延迟更低）
- **> 20 KB** → `br`（压缩率高 15-25%，大响应带宽节省显著）
- 客户端不支持某算法或服务端关闭时，自动降级到下一个

---

## 全局配置

```toml
[global.compress]
gzip         = true    # 启用 gzip（默认 true）
gzip_level   = 6       # 1-9，默认 6（均衡点，等价 Nginx gzip_comp_level 6）
brotli       = true    # 启用 brotli（默认 true）
brotli_level = 4       # 0-11，默认 4（速度/压缩率均衡）
zstd         = true    # 启用 zstd（默认 true）
zstd_level   = 3       # 1-22，默认 3（极速）
min_length   = 1       # 触发压缩的最小文件大小（KB，仅影响静态文件）
```

### 字段说明

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `gzip` | `true` | 是否启用 gzip 压缩 |
| `gzip_level` | `6` | gzip 压缩等级 1-9，6 为均衡点（Nginx 默认也是 6） |
| `brotli` | `true` | 是否启用 brotli 压缩 |
| `brotli_level` | `4` | brotli 压缩等级 0-11，4 兼顾速度与压缩率 |
| `zstd` | `true` | 是否启用 zstd 压缩 |
| `zstd_level` | `3` | zstd 压缩等级 1-22，3 为极速默认值 |
| `min_length` | `1` KB | 触发压缩的最小文件大小，小于此值的文件不压缩（仅影响静态文件） |

---

## 站点级覆盖

通过 `[sites.compress]` 可针对单个站点覆盖全局配置，未设置的字段继承全局：

```toml
[[sites]]
name        = "api-server"
server_name = ["api.example.com"]

[sites.compress]
gzip         = false   # 关闭 gzip（API 客户端通常只用 zstd/br）
brotli       = true
brotli_level = 6       # 提高压缩率（API 响应体积小，CPU 开销可接受）
zstd         = true
zstd_level   = 6       # 提高压缩率
```

**继承规则**：未在 `[sites.compress]` 中设置的字段，使用 `[global.compress]` 的值。

---

## 各场景行为详解

### 静态文件

首次请求时，Sweety 同时生成 gzip、brotli、zstd 三份压缩结果存入内存缓存。后续请求直接返回对应缓存，**零 CPU 开销**：

```
第一次请求 → 读文件 → 并行生成 br/zstd/gz 缓存 → 返回最优编码
后续请求   → 查内存缓存 → 直接返回（0 CPU）
```

- **缓存上限**：单文件 ≤ 1 MB，总缓存 64 MB，最多 2048 个条目
- **文件变更**：watcher 检测到修改/删除自动清除对应缓存条目
- **不可压缩格式**：`.gz`、`.br`、`.zst`、`.zip`、`.png`、`.jpg`、`.webp`、`.mp4`、`.woff2` 等不再压缩

**大文件（> 1 MB）** 走流式压缩，实时编码不缓冲全量 body：

```toml
# 可通过 min_length 调整触发阈值（默认 1 KB，静态文件启用压缩的下限）
[global.compress]
min_length = 4   # 4 KB 以上才压缩（单位：KB）
```

### 反向代理

Sweety 的 `Accept-Encoding` 头会**透传给上游**，上游可自行决定是否压缩：

- 上游已压缩（响应包含 `Content-Encoding`）→ Sweety 直接透传，**不重复压缩**
- 上游未压缩 → Sweety 检查配置，按 br > zstd > gzip 优先级对响应流式压缩后返回客户端

压缩决策逻辑：

```
1. 全部算法关闭？→ 直接透传（零开销）
2. 上游已有 Content-Encoding？→ 直接透传（避免双重压缩）
3. Content-Type 不可压缩？→ 直接透传（图片/二进制等）
4. 客户端 Accept-Encoding 无匹配？→ 直接透传
5. 以上均通过 → 流式压缩响应 body，设置 Content-Encoding + Vary: Accept-Encoding
```

### PHP FastCGI

PHP 响应的压缩逻辑与反向代理**完全一致**，均使用相同的 `effective_compress` + `compress_response` 函数链：

- PHP 已输出 `Content-Encoding`（如 `ob_gzhandler`）→ 不重复压缩
- PHP 输出 `text/html`、`application/json` 等可压缩 mime → 流式压缩
- PHP 输出图片、二进制 → 不压缩

> **建议**：如果 PHP 开启了 `zlib.output_compression` 或使用 `ob_gzhandler`，请关闭以避免双重压缩，或将 `[sites.compress]` 全部设为 `false`。

---

## 关闭压缩

### 全局关闭

```toml
[global.compress]
gzip   = false
brotli = false
zstd   = false
```

### 单站点关闭

```toml
[sites.compress]
gzip   = false
brotli = false
zstd   = false
```

### 仅对代理/PHP 关闭（静态文件保持压缩）

目前无独立开关区分场景，关闭 `[sites.compress]` 会同时关闭该站点所有场景的压缩。

---

## Accept-Encoding 内容协商

Sweety 按 **RFC 7231 §5.3.4** 严格解析 `Accept-Encoding` 头：

- 支持 `q=` 权重（0.0–1.0），缺省为 1.0
- 支持 `*` 通配符（为所有未显式列出的编码设置默认 q）
- 支持 `identity;q=0`（客户端明确拒绝未压缩传输）
- 大小写不敏感

示例（假设三种算法均开启）：

| Accept-Encoding | 响应 10 KB | 响应 100 KB | 说明 |
|-----------------|------------|-------------|------|
| `gzip, deflate, br, zstd` | `zstd` | `br` | 无偏好，自适应选择 |
| `gzip;q=0.9, br;q=1.0` | `br` | `br` | 客户端显式偏好 br |
| `gzip;q=0.9, zstd;q=0.8, br;q=0.7` | `gzip` | `gzip` | 客户端显式偏好 gzip |
| `gzip;q=0, *;q=0.5` | `zstd`或`br` | `br` | gzip 被拒绝，从剩余自适应选 |
| `identity` | 无压缩 | 无压缩 | 仅接受原始内容 |

---

## 响应头

压缩响应会携带以下头：

| 响应头 | 说明 |
|--------|------|
| `Content-Encoding: br/zstd/gzip` | 实际使用的压缩算法 |
| `Vary: Accept-Encoding` | 告知 CDN/代理按编码分别缓存（RFC 7231 §7.1.4）|

> **CDN/代理缓存**：`Vary: Accept-Encoding` 确保不同 `Accept-Encoding` 的客户端获得正确版本，避免 CDN 将压缩内容返回给不支持压缩的客户端。

---

## 压缩等级选择参考

### gzip（1-9）

| 等级 | 适用场景 |
|------|----------|
| 1-3 | 高频变化的 API 响应，速度优先 |
| **6** | **默认，均衡点**（等价 Nginx 默认） |
| 7-9 | 静态资源，压缩率优先（Sweety 静态文件用预压缩，此等级影响大文件流式压缩） |

### brotli（0-11）

| 等级 | 适用场景 |
|------|----------|
| 0-3 | 动态响应，接近 gzip 速度 |
| **4** | **默认，速度/压缩率均衡** |
| 9-11 | 静态资源预压缩（CPU 开销大，适合离线生成） |

### zstd（1-22）

| 等级 | 适用场景 |
|------|----------|
| **3** | **默认，极速**（比 gzip 快 5-10×，压缩率不低于 gzip） |
| 7-12 | 均衡场景 |
| 15+ | 压缩率优先，开销显著上升 |

---

## 向后兼容（旧 gzip 字段）

以下旧字段仍受支持，优先级低于 `[global.compress]` / `[sites.compress]`：

```toml
# 全局旧字段
gzip            = true    # 等价 global.compress.gzip（优先级低）
gzip_min_length = 1       # 等价 global.compress.min_length
gzip_comp_level = 6       # 等价 global.compress.gzip_level

# 站点旧字段
gzip            = true    # 等价 sites.compress.gzip（优先级低）
gzip_comp_level = 6       # 等价 sites.compress.gzip_level
```

> 如果同时配置了旧字段和 `[*.compress]`，**`[*.compress]` 优先**。建议迁移到新字段。
