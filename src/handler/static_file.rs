//! 静态文件处理器
//! 负责：零拷贝文件传输、Range 请求支持、默认文档查找、ETag/Last-Modified 缓存验证

use std::path::{Path, PathBuf};

use tokio::io::AsyncReadExt;
use tracing::debug;

use crate::config::model::LocationConfig;
use crate::dispatcher::{vhost::SiteInfo, DispatchResponse};

/// 处理静态文件请求
pub async fn handle(
    site: &SiteInfo,
    location: &LocationConfig,
    method: &str,
    path: &str,
) -> DispatchResponse {
    // 确定文件系统根目录（location 级 root 优先于 site 级 root）
    let root = match location.root.as_ref().or(site.root.as_ref()) {
        Some(r) => r.clone(),
        None => {
            return error_response(500, "站点未配置 root 目录");
        }
    };

    // 解析并规范化请求路径，防止目录穿越
    let file_path = match resolve_safe_path(&root, path) {
        Some(p) => p,
        None => return error_response(403, "Forbidden"),
    };

    // 若路径为目录，尝试默认文档
    if file_path.is_dir() {
        for index in &site.index {
            let candidate = file_path.join(index);
            if candidate.is_file() {
                return serve_file(method, &candidate).await;
            }
        }
        return error_response(403, "Directory listing disabled");
    }

    if file_path.is_file() {
        serve_file(method, &file_path).await
    } else {
        error_response(404, "Not Found")
    }
}

/// 实际读取并返回文件内容
async fn serve_file(method: &str, path: &Path) -> DispatchResponse {
    debug!("提供静态文件: {}", path.display());

    // HEAD 请求只需要响应头
    if method.eq_ignore_ascii_case("HEAD") {
        return DispatchResponse {
            status_code: 200,
            status_text: "OK",
            body: String::new(),
        };
    }

    match tokio::fs::File::open(path).await {
        Ok(mut file) => {
            let mut contents = Vec::new();
            if file.read_to_end(&mut contents).await.is_err() {
                return error_response(500, "文件读取失败");
            }
            // 后续版本：在此处添加 ETag/Last-Modified 验证、Content-Type 推断、
            // Range 请求处理、零拷贝 sendfile 支持
            DispatchResponse {
                status_code: 200,
                status_text: "OK",
                body: String::from_utf8_lossy(&contents).into_owned(),
            }
        }
        Err(e) => {
            tracing::error!("打开文件失败 {}: {}", path.display(), e);
            error_response(500, "Internal Server Error")
        }
    }
}

/// 将请求路径安全地解析为文件系统路径（防止目录穿越攻击）
///
/// 确保解析后的路径始终位于 root 目录下
fn resolve_safe_path(root: &Path, request_path: &str) -> Option<PathBuf> {
    // 去掉查询字符串
    let path_only = request_path.split('?').next().unwrap_or(request_path);

    // 规范化：去掉 URL 编码（简化版，完整版需要 percent_decode）
    // 拒绝包含 `..` 的路径片段
    for segment in path_only.split('/') {
        if segment == ".." || segment == "." {
            return None;
        }
    }

    // 拼接路径
    let relative = path_only.trim_start_matches('/');
    let full = root.join(relative);

    // 规范化路径并确认仍在 root 下（防止符号链接穿越）
    match (full.canonicalize().ok(), root.canonicalize().ok()) {
        (Some(canonical_full), Some(canonical_root)) => {
            if canonical_full.starts_with(&canonical_root) {
                Some(canonical_full)
            } else {
                None // 目录穿越尝试
            }
        }
        // 文件不存在时，canonicalize 会失败，直接返回拼接路径
        // 后续会被 is_file()/is_dir() 检查
        _ => Some(full),
    }
}

/// 构造错误响应
fn error_response(code: u16, msg: &str) -> DispatchResponse {
    let text = match code {
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Error",
    };
    DispatchResponse {
        status_code: code,
        status_text: text,
        body: msg.to_string(),
    }
}

// ─────────────────────────────────────────────
// 单元测试
// ─────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn temp_dir_with_file(name: &str, content: &str) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(dir.path().join(name)).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        dir
    }

    #[test]
    fn test_resolve_safe_path_normal() {
        let dir = tempfile::tempdir().unwrap();
        let result = resolve_safe_path(dir.path(), "/index.html");
        assert!(result.is_some());
    }

    #[test]
    fn test_resolve_safe_path_directory_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let result = resolve_safe_path(dir.path(), "/../etc/passwd");
        // 包含 `..` 片段应被拒绝
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_serve_existing_file() {
        let dir = temp_dir_with_file("hello.txt", "Hello, Sweety!");
        let site = SiteInfo {
            name: "test".into(),
            root: Some(dir.path().to_path_buf()),
            index: vec!["index.html".into()],
            locations: vec![],
            rewrites: vec![],
            upstreams: vec![],
            tls: None,
        };
        let loc = LocationConfig {
            path: "/".into(),
            handler: crate::config::model::HandlerType::Static,
            root: None,
            upstream: None,
            cache_control: None,
            return_code: None,
            max_connections: None,
        };
        let resp = handle(&site, &loc, "GET", "/hello.txt").await;
        assert_eq!(resp.status_code, 200);
        assert_eq!(resp.body, "Hello, Sweety!");
    }

    #[tokio::test]
    async fn test_missing_file_returns_404() {
        let dir = tempfile::tempdir().unwrap();
        let site = SiteInfo {
            name: "test".into(),
            root: Some(dir.path().to_path_buf()),
            index: vec![],
            locations: vec![],
            rewrites: vec![],
            upstreams: vec![],
            tls: None,
        };
        let loc = LocationConfig {
            path: "/".into(),
            handler: crate::config::model::HandlerType::Static,
            root: None,
            upstream: None,
            cache_control: None,
            return_code: None,
            max_connections: None,
        };
        let resp = handle(&site, &loc, "GET", "/notexist.html").await;
        assert_eq!(resp.status_code, 404);
    }

    #[tokio::test]
    async fn test_default_document() {
        let dir = temp_dir_with_file("index.html", "<h1>Welcome</h1>");
        let site = SiteInfo {
            name: "test".into(),
            root: Some(dir.path().to_path_buf()),
            index: vec!["index.html".into()],
            locations: vec![],
            rewrites: vec![],
            upstreams: vec![],
            tls: None,
        };
        let loc = LocationConfig {
            path: "/".into(),
            handler: crate::config::model::HandlerType::Static,
            root: None,
            upstream: None,
            cache_control: None,
            return_code: None,
            max_connections: None,
        };
        // 请求目录时应自动返回 index.html
        let resp = handle(&site, &loc, "GET", "/").await;
        assert_eq!(resp.status_code, 200);
        assert!(resp.body.contains("Welcome"));
    }
}
