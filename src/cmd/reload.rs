//! `sweety reload` —— 热重载配置（向 Admin API 发送 POST /api/config/reload）

use std::path::PathBuf;

use tracing::info;

use crate::util::{init_stderr_log, load_cfg_or_exit};

pub fn cmd_reload(config: &PathBuf) {
    init_stderr_log();
    let cfg = load_cfg_or_exit(config);
    let addr = &cfg.global.admin_listen;
    if addr.is_empty() {
        eprintln!("[ERROR] global.admin_listen 未配置，无法发送 reload 信号");
        eprintln!("请在配置文件中添加: admin_listen = \"127.0.0.1:9000\"");
        std::process::exit(1);
    }
    let url = format!("http://{}/api/config/reload", addr);
    info!("发送热重载请求到: {}", url);
    let token = &cfg.global.admin_token;
    match http_post(&url, token) {
        Ok(_)  => info!("热重载成功"),
        Err(e) => {
            eprintln!("[ERROR] 热重载失败: {}", e);
            std::process::exit(1);
        }
    }
}

/// 极简 HTTP POST（标准库实现，无需 reqwest/tokio）
fn http_post(url: &str, token: &str) -> Result<(), String> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let url = url.strip_prefix("http://").unwrap_or(url);
    let (host_port, path) = url.split_once('/').unwrap_or((url, "api/v1/reload"));
    let path = format!("/{}", path);

    let mut stream = TcpStream::connect(host_port)
        .map_err(|e| format!("连接 {} 失败: {}", host_port, e))?;

    let auth = if token.is_empty() {
        String::new()
    } else {
        format!("Authorization: Bearer {}\r\n", token)
    };
    let req = format!(
        "POST {} HTTP/1.0\r\nHost: {}\r\nContent-Length: 0\r\n{}\r\n",
        path, host_port, auth
    );
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;

    let mut resp = String::new();
    stream.read_to_string(&mut resp).map_err(|e| e.to_string())?;
    if resp.starts_with("HTTP/1") && (resp.contains("200") || resp.contains("204")) {
        Ok(())
    } else {
        Err(format!("服务器返回: {}", resp.lines().next().unwrap_or("")))
    }
}
