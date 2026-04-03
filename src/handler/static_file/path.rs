//! 请求路径安全解析及 try_files 逻辑

use std::path::{Path, PathBuf};

// ─────────────────────────────────────────────
// try_files
// ─────────────────────────────────────────────

/// try_files 解析结果
pub enum TryFilesResult {
    /// 找到可用文件（静态文件或 PHP 脚本，调用方根据扩展名分流）
    File(PathBuf),
    /// 返回指定错误码（如 =404）
    Code(u16),
    /// 所有路径均不存在
    NotFound,
}

/// 按 try_files 列表依次尝试路径（等价 Nginx try_files $uri $uri/ /fallback.html =404）
pub async fn try_files_resolve(
    try_files_list: &[String],
    request_path: &str,
    root: Option<&std::path::PathBuf>,
) -> TryFilesResult {
    let index_files = vec!["index.php".to_string(), "index.html".to_string()];
    let root_path = match root {
        Some(r) => r.as_path(),
        None => return TryFilesResult::NotFound,
    };
    try_files_resolve_inner(root_path, request_path, try_files_list, &index_files).await
}

async fn try_files_resolve_inner(
    root: &Path,
    request_path: &str,
    try_files: &[String],
    index_files: &[String],
) -> TryFilesResult {
    let uri = request_path.split('?').next().unwrap_or(request_path);

    for entry in try_files {
        let entry = entry.trim();

        if let Some(code_str) = entry.strip_prefix('=') {
            if let Ok(code) = code_str.parse::<u16>() {
                return TryFilesResult::Code(code);
            }
            continue;
        }

        let expanded = entry
            .replace("$uri", uri)
            .replace("$request_uri", request_path);

        if expanded.ends_with('/') {
            let dir_path = match resolve_safe_path(root, expanded.trim_end_matches('/')) {
                Some(p) => p,
                None => continue,
            };
            if let Some(idx) = find_index(&dir_path, index_files).await {
                return TryFilesResult::File(idx);
            }
        } else {
            let file_path = match resolve_safe_path(root, &expanded) {
                Some(p) => p,
                None => continue,
            };
            if file_path.is_file() {
                return TryFilesResult::File(file_path);
            }
        }
    }

    TryFilesResult::NotFound
}

async fn find_index(dir: &Path, index_files: &[String]) -> Option<PathBuf> {
    for name in index_files {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

// ─────────────────────────────────────────────
// 路径安全解析（防目录穿越）
// ─────────────────────────────────────────────

/// 将请求路径安全地解析为文件系统绝对路径
pub fn resolve_safe_path(root: &Path, request_path: &str) -> Option<PathBuf> {
    resolve_safe_path_with_canon(root, request_path, None)
}

/// 带预计算 canonical root 的版本（供 handle_sweety 调用以消除重复系统调用）
pub fn resolve_safe_path_fast(
    root: &Path,
    request_path: &str,
    canonical_root: Option<&Path>,
) -> Option<PathBuf> {
    resolve_safe_path_with_canon(root, request_path, canonical_root)
}

fn resolve_safe_path_with_canon(
    root: &Path,
    request_path: &str,
    canonical_root: Option<&Path>,
) -> Option<PathBuf> {
    let path_only = request_path.split('?').next().unwrap_or(request_path);

    for segment in path_only.split('/') {
        if segment == ".." { return None; }
    }

    let relative = path_only.trim_start_matches('/');
    let full = root.join(relative);

    let cr_opt: Option<PathBuf>;
    let cr = match canonical_root {
        Some(p) => Some(p),
        None => {
            cr_opt = root.canonicalize().ok();
            cr_opt.as_deref()
        }
    };
    match (full.canonicalize().ok(), cr) {
        (Some(cf), Some(cr)) => {
            if cf.starts_with(cr) { Some(cf) } else { None }
        }
        _ => Some(full),
    }
}
