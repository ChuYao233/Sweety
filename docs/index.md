---
layout: home

hero:
  name: Sweety
  text: 高性能多站点 Web 服务器
  tagline: 基于 Rust，兼顾 Nginx 深度配置与 Caddy 开箱即用体验。支持 HTTP/3、ACME 自动证书、FastCGI、反向代理、gRPC、WebSocket、热重载。
  actions:
    - theme: brand
      text: 快速开始
      link: /getting-started/quickstart
    - theme: alt
      text: 配置参考
      link: /configuration/overview
    - theme: alt
      text: GitHub
      link: https://github.com/ChuYao233/Sweety

features:
  - icon: ⚡
    title: 极致性能
    details: 小文件高并发 RPS 比 Nginx 高 +100%，P99 尾延迟低 94%，零 GOAWAY 错误。SO_REUSEPORT 多核扩展，H2 per-connection writer loop 消除 HOL blocking。

  - icon: 🔒
    title: 自动 HTTPS
    details: 内置 ACME，一行 acme_email 开启 Let's Encrypt / ZeroSSL 自动证书。支持 HTTP-01 与 DNS-01（Cloudflare / 阿里云 / Shell）通配符证书。

  - icon: 🌐
    title: HTTP/1.1 · H2 · H3
    details: 原生支持 HTTP/3 / QUIC，与 H2 共享 443 端口，无需重编译。浏览器自动通过 Alt-Svc 升级，高丢包/延迟网络体验提升显著。

  - icon: 🎯
    title: 开箱即用
    details: preset = "wordpress" 一行自动生成最优 location 规则；php_fastcgi = "/tmp/php.sock" 一行代替完整 FastCGI 配置块；Caddy 式语法糖，8 行配置跑起来 WordPress。

  - icon: 🔄
    title: 反向代理 & gRPC
    details: 支持轮询 / 加权 / 最少连接 / IP 哈希负载均衡，连接池，断路器（三状态机），主动健康检查，gRPC 透明转发，WebSocket 全透传。

  - icon: 🛠️
    title: 零停机运维
    details: sweety reload 热重载配置，不断开现有连接。Admin REST API + Prometheus /metrics，配置验证（sweety validate），Daemon 模式开箱即用。
---

> ⚠️ **注意**：Sweety 目前仍处于积极开发阶段，尚未经过生产环境验证。欢迎在测试/开发环境试用并[反馈问题](https://github.com/ChuYao233/Sweety/issues)。
