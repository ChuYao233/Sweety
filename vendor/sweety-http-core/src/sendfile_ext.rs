/// HTTP 响应扩展：H1 dispatcher 检测此 extension 后走 sendfile(2) 零拷贝路径。
///
/// 设置方法：在 handler 返回响应前将此 struct 插入 response extensions：
/// ```ignore
/// resp.extensions_mut().insert(SendFileInfo { fd: arc_fd, offset: 0, len: file_size });
/// ```
/// 如果连接是 TLS 或者平台不支持 sendfile，dispatcher 会忽略此 extension，
/// 回退到正常的 body stream 路径（response body 应同时设置为 pread_stream）。
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct SendFileInfo {
    pub fd:     std::sync::Arc<std::fs::File>,
    pub offset: u64,
    pub len:    u64,
}

/// Linux sendfile(2)：把文件 fd 的 [offset, offset+len) 字节写入 AsyncIo socket，零用户态拷贝。
///
/// `#[allow(unsafe_code)]` 局部覆盖模块级 deny，仅此函数使用 unsafe。
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub async fn sendfile_to_io<St: sweety_io_compat::io::AsyncIo>(
    io: &mut St,
    sock_fd: i32,
    file: &std::sync::Arc<std::fs::File>,
    offset: u64,
    len: u64,
) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    use sweety_io_compat::io::Interest;

    let file_fd = file.as_raw_fd();
    let mut off = offset as libc::off_t;
    let mut rem = len as usize;

    while rem > 0 {
        io.ready(Interest::WRITABLE).await?;
        let n = unsafe {
            libc::sendfile(sock_fd, file_fd, &mut off, rem.min(1 << 21))
        };
        match n {
            n if n > 0 => rem -= n as usize,
            0 => break,
            _ => {
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::WouldBlock {
                    return Err(e);
                }
            }
        }
    }
    Ok(())
}
