# Sweety 文档

Sweety 是基于 Rust 的高性能多站点 Web 服务器，支持 HTTP/1.1、HTTP/2、HTTP/3（QUIC）、TLS/ACME、FastCGI、反向代理、gRPC、WebSocket、热重载。

## 目录

### 入门
- [简介与特性](getting-started/introduction.md)
- [编译与安装](getting-started/installation.md)
- [快速开始](getting-started/quickstart.md)
- [常见问题](getting-started/faq.md)

### 配置参考
- [配置文件概述](configuration/overview.md)
- [全局配置 \[global\]](configuration/global.md)
- [站点配置 \[\[sites\]\]](configuration/sites.md)
- [TLS / HTTPS / ACME](configuration/tls.md)
- [路由规则 locations](configuration/locations.md)
- [FastCGI / PHP](configuration/fastcgi.md)
- [反向代理](configuration/reverse-proxy.md)
- [gRPC 代理](configuration/grpc.md)
- [WebSocket](configuration/websocket.md)
- [缓存](configuration/cache.md)
- [速率限制](configuration/rate-limit.md)
- [内置预设](configuration/presets.md)
- [HTTP/3 调优](configuration/http3.md)

### 命令行
- [命令行参考](cli.md)

### 示例
- [WordPress](examples/wordpress.md)
- [Laravel](examples/laravel.md)
- [静态站点](examples/static-site.md)
- [反向代理](examples/reverse-proxy.md)
- [gRPC](examples/grpc.md)

### 进阶
- [性能测试](performance.md)
- [热重载](advanced/hot-reload.md)
- [Roadmap](roadmap.md)
