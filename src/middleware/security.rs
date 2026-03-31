//! 安全策略中间件
//! 负责：敏感文件路径拦截、自动注入安全响应头（HSTS/CSP/X-Frame-Options 等）

/// 敏感文件名 phf 完美哈希表（全小写 key，O(1) 查找）
static SENSITIVE_SET: phf::Set<&'static str> = phf::phf_set! {
    ".git", ".env", ".htaccess", ".htpasswd", ".ds_store",
    "composer.json", "composer.lock", "package.json", "package-lock.json",
    "yarn.lock", "makefile", "dockerfile", ".dockerignore",
    "wp-config.php", "config.php", ".ssh", ".aws",
};

/// 检查请求路径是否命中敏感文件拦截规则。返回 `true` 表示应返回 403。
pub fn is_sensitive_path(path: &str) -> bool {
    // 去掉 query string（不含 '?'时直接用原字符串）
    let path_only = match path.find('?') {
        Some(i) => &path[..i],
        None    => path,
    };

    // 按 '/' 分段，每段转小写后查 phf
    for segment in path_only.split('/') {
        if segment.is_empty() { continue; }
        // 首字符快速过滤：敏感名以 '.' / 'c' / 'd' / 'm' / 'p' / 'w' / 'y' 开头
        let first = segment.as_bytes()[0].to_ascii_lowercase();
        if !matches!(first, b'.' | b'c' | b'd' | b'm' | b'p' | b'w' | b'y') {
            continue;
        }
        // 小写转换（根段通常很短，用栈分配）
        let mut buf = [0u8; 64];
        let bytes = segment.as_bytes();
        if bytes.len() <= 64 {
            for (i, &b) in bytes.iter().enumerate() {
                buf[i] = b.to_ascii_lowercase();
            }
            if let Ok(lower) = std::str::from_utf8(&buf[..bytes.len()]) {
                if SENSITIVE_SET.contains(lower) { return true; }
            }
        }
    }
    false
}

/// 安全响应头配置
#[derive(Debug, Clone)]
pub struct SecurityHeaders {
    /// 是否启用 HSTS（仅 HTTPS 站点应启用）
    pub hsts: bool,
    /// HSTS max-age（秒）
    pub hsts_max_age: u64,
    /// X-Frame-Options 值（DENY / SAMEORIGIN）
    pub x_frame_options: Option<String>,
    /// X-Content-Type-Options（nosniff）
    pub x_content_type_options: bool,
    /// X-XSS-Protection
    pub x_xss_protection: bool,
    /// Content-Security-Policy
    pub csp: Option<String>,
    /// Referrer-Policy
    pub referrer_policy: Option<String>,
}

impl Default for SecurityHeaders {
    fn default() -> Self {
        Self {
            hsts: false,
            hsts_max_age: 31536000, // 1 年
            x_frame_options: Some("SAMEORIGIN".into()),
            x_content_type_options: true,
            x_xss_protection: true,
            csp: None,
            referrer_policy: Some("strict-origin-when-cross-origin".into()),
        }
    }
}

impl SecurityHeaders {
    /// 生成需要注入的响应头列表（(header_name, header_value)）
    pub fn to_headers(&self) -> Vec<(&'static str, String)> {
        let mut headers = Vec::new();

        if self.hsts {
            headers.push((
                "Strict-Transport-Security",
                format!("max-age={}; includeSubDomains", self.hsts_max_age),
            ));
        }

        if let Some(x_frame) = &self.x_frame_options {
            headers.push(("X-Frame-Options", x_frame.clone()));
        }

        if self.x_content_type_options {
            headers.push(("X-Content-Type-Options", "nosniff".into()));
        }

        if self.x_xss_protection {
            headers.push(("X-XSS-Protection", "1; mode=block".into()));
        }

        if let Some(csp) = &self.csp {
            headers.push(("Content-Security-Policy", csp.clone()));
        }

        if let Some(rp) = &self.referrer_policy {
            headers.push(("Referrer-Policy", rp.clone()));
        }

        // 始终隐藏服务器信息
        headers.push(("Server", "Sweety".into()));

        headers
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sensitive_git_path() {
        assert!(is_sensitive_path("/.git"));
        assert!(is_sensitive_path("/.git/config"));
        assert!(is_sensitive_path("/any/dir/.git/HEAD"));
    }

    #[test]
    fn test_sensitive_env_file() {
        assert!(is_sensitive_path("/.env"));
        assert!(is_sensitive_path("/.htaccess"));
        assert!(is_sensitive_path("/wp-config.php"));
    }

    #[test]
    fn test_normal_path_allowed() {
        assert!(!is_sensitive_path("/index.html"));
        assert!(!is_sensitive_path("/api/users"));
        assert!(!is_sensitive_path("/static/app.js"));
        assert!(!is_sensitive_path("/about.php"));
    }

    #[test]
    fn test_sensitive_path_ignores_query_string() {
        assert!(is_sensitive_path("/.env?foo=bar"));
    }

    #[test]
    fn test_security_headers_default() {
        let headers = SecurityHeaders::default();
        let list = headers.to_headers();
        let names: Vec<&str> = list.iter().map(|(k, _)| *k).collect();
        assert!(names.contains(&"X-Frame-Options"));
        assert!(names.contains(&"X-Content-Type-Options"));
        assert!(names.contains(&"Server"));
        // 默认 HSTS 关闭
        assert!(!names.contains(&"Strict-Transport-Security"));
    }

    #[test]
    fn test_security_headers_with_hsts() {
        let mut headers = SecurityHeaders::default();
        headers.hsts = true;
        let list = headers.to_headers();
        let hsts = list.iter().find(|(k, _)| *k == "Strict-Transport-Security");
        assert!(hsts.is_some());
        assert!(hsts.unwrap().1.contains("max-age=31536000"));
    }

    #[test]
    fn test_server_header_always_sweety() {
        let headers = SecurityHeaders::default();
        let list = headers.to_headers();
        let server = list.iter().find(|(k, _)| *k == "Server");
        assert_eq!(server.unwrap().1, "Sweety");
    }
}
