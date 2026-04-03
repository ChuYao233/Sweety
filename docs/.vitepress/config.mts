import { defineConfig } from 'vitepress'

export default defineConfig({
  lang: 'zh-CN',
  title: 'Sweety',
  description: '高性能多站点 Web 服务器 —— 基于 Rust，兼顾 Nginx 深度配置与 Caddy 开箱即用体验',

  head: [
    ['link', { rel: 'icon', type: 'image/svg+xml', href: '/logo.svg' }],
  ],

  themeConfig: {
    logo: '/logo.svg',
    siteTitle: 'Sweety',

    // ─── 侧边栏（全局统一） ────────────────────────────────────────
    sidebar: [
      {
        text: '入门',
        items: [
          { text: '简介与特性', link: '/getting-started/introduction' },
          { text: '编译与安装', link: '/getting-started/installation' },
          { text: '快速开始', link: '/getting-started/quickstart' },
          { text: '常见问题', link: '/getting-started/faq' },
        ],
      },
      {
        text: '配置',
        items: [
          { text: '概述', link: '/configuration/overview' },
          { text: '全局配置 [global]', link: '/configuration/global' },
          { text: '站点配置 [[sites]]', link: '/configuration/sites' },
          { text: 'TLS / ACME', link: '/configuration/tls' },
          { text: 'HTTP/3 调优', link: '/configuration/http3' },
          { text: '路由规则 locations', link: '/configuration/locations' },
          { text: 'FastCGI / PHP', link: '/configuration/fastcgi' },
          { text: '反向代理', link: '/configuration/reverse-proxy' },
          { text: 'gRPC 代理', link: '/configuration/grpc' },
          { text: 'WebSocket', link: '/configuration/websocket' },
          { text: '缓存', link: '/configuration/cache' },
          { text: '速率限制', link: '/configuration/rate-limit' },
          { text: '内置预设', link: '/configuration/presets' },
        ],
      },
      {
        text: '示例',
        items: [
          { text: 'WordPress / Laravel', link: '/examples/wordpress' },
          { text: '静态站点 / SPA', link: '/examples/static-site' },
          { text: '反向代理 / gRPC', link: '/examples/reverse-proxy' },
        ],
      },
      {
        text: '更多',
        items: [
          { text: '命令行参考', link: '/cli' },
          { text: '性能测试', link: '/performance' },
          { text: 'Roadmap', link: '/roadmap' },
          { text: '热重载', link: '/advanced/hot-reload' },
        ],
      },
    ],

    // ─── 页面底部 ─────────────────────────────────────────────────
    socialLinks: [
      { icon: 'github', link: 'https://github.com/ChuYao233/Sweety' },
    ],

    footer: {
      message: '基于 Apache License 2.0 发布',
    },

    // ─── 搜索 ─────────────────────────────────────────────────────
    search: {
      provider: 'local',
    },

    // ─── 编辑链接 ─────────────────────────────────────────────────
    editLink: {
      pattern: 'https://github.com/ChuYao233/Sweety/edit/main/docs/:path',
      text: '在 GitHub 上编辑此页',
    },

    // ─── 本地化文案 ───────────────────────────────────────────────
    outline: {
      label: '本页目录',
      level: [2, 3],
    },

    docFooter: {
      prev: '上一篇',
      next: '下一篇',
    },

    lastUpdated: {
      text: '最后更新',
    },

    returnToTopLabel: '回到顶部',
    sidebarMenuLabel: '目录',
    darkModeSwitchLabel: '深色模式',
  },
})
