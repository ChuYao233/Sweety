import DefaultTheme from 'vitepress/theme'
import './custom.css'
import { useRouter } from 'vitepress'
import { onMounted } from 'vue'

// 浏览器语言 → 站点语言前缀映射（可扩展更多语言）
const LANG_PREFIX_MAP: Record<string, string> = {
  'en': '/en-us/',
  // 'ja': '/ja/',
  // 'ko': '/ko/',
}

// 默认语言前缀（中文为根目录，无前缀）
const DEFAULT_PREFIX = '/'

// 已支持的语言前缀列表
const SUPPORTED_PREFIXES = ['/', '/en-us/']

function detectAndRedirect() {
  // 仅在客户端执行
  if (typeof window === 'undefined') return

  // 如果已经访问过，不再跳转
  const visited = localStorage.getItem('sweety-docs-lang-redirected')
  if (visited) return

  // 标记已访问
  localStorage.setItem('sweety-docs-lang-redirected', '1')

  const path = window.location.pathname

  // 如果当前已在非根语言路径，说明用户主动选择了语言，不跳转
  for (const prefix of SUPPORTED_PREFIXES) {
    if (prefix !== '/' && path.startsWith(prefix)) return
  }

  // 只在根语言（中文）页面时检测是否需要跳转
  const lang = navigator.language?.toLowerCase() || ''

  // 匹配浏览器语言到站点语言
  for (const [browserLang, sitePrefix] of Object.entries(LANG_PREFIX_MAP)) {
    if (lang.startsWith(browserLang) && sitePrefix !== DEFAULT_PREFIX) {
      // 将当前路径映射到目标语言路径
      const targetPath = sitePrefix + path.replace(/^\//, '')
      window.location.pathname = targetPath
      return
    }
  }
}

export default {
  extends: DefaultTheme,
  setup() {
    onMounted(() => {
      detectAndRedirect()
    })
  },
}
