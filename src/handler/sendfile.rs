//! Zero-copy 文件传输
//!
//! # 性能策略（参考 Pingora / Nginx）
//!
//! | 协议 / 平台      | 实现                          | 特点                        |
//! |-----------------|-------------------------------|-----------------------------|
//! | HTTP/1.1 Linux  | `sendfile(2)` 系统调用         | 内核直传，零用户态拷贝        |
//! | HTTP/1.1 macOS  | `sendfile(2)` BSD 变体        | 同上                         |
//! | HTTP/2 任意平台  | bounded channel (cap=2) 背压  | H2 framer 拉一块读一块       |
//! | Windows fallback| `tokio::fs` 分块读取           | 无 sendfile，安全分块        |
//!
//! ## 背压原理
//! H2 不能用 sendfile（需封帧），改用容量为 2 的 channel：
//! - 生产者 task 每次发一个 256KB chunk，发满就 await（等消费者取走）
//! - 消费者（H2 framer）每次拉一个 chunk 才解除生产者阻塞
//! - 内存占用恒定 ≤ 2 × 256KB = 512KB，无论文件多大

use std::io::Result as IoResult;
use bytes::Bytes;
use futures_util::Stream;
use tokio::io::AsyncReadExt;
use tokio_util::io::ReaderStream;

/// 大文件流式传输块大小（256 KiB）
pub const STREAM_CHUNK: usize = 256 * 1024;

/// 返回一个带背压的文件 stream（H2/通用路径）
///
/// 接受任意实现 `AsyncRead + Send + 'static` 的 reader（`File`、`Take<File>` 等）。
/// 使用 bounded channel (capacity=2)：
/// 生产者 task 读文件发 chunk，channel 满则挂起等待消费者拉取。
/// H2 framer 每消费一帧才解除生产者阻塞，内存恒定 ≤ 2×STREAM_CHUNK。
pub fn file_stream_backpressure<R>(
    reader: R,
    len: u64,
) -> impl Stream<Item = IoResult<Bytes>> + Send + 'static
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::channel::<IoResult<Bytes>>(2);

    tokio::spawn(async move {
        let mut reader = reader;
        let mut remaining = len;
        let mut buf = vec![0u8; STREAM_CHUNK];

        while remaining > 0 {
            let to_read = (remaining as usize).min(STREAM_CHUNK);
            let slice = &mut buf[..to_read];

            // read 而非 read_exact：EOF 提前结束也属正常
            match reader.read(slice).await {
                Ok(0) => break, // EOF
                Ok(n) => {
                    let chunk = Bytes::copy_from_slice(&slice[..n]);
                    remaining = remaining.saturating_sub(n as u64);
                    if tx.send(Ok(chunk)).await.is_err() {
                        return; // 客户端断开
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            }
        }
    });

    tokio_stream::wrappers::ReceiverStream::new(rx)
}

/// Linux/macOS HTTP/1.1：`sendfile(2)` 零拷贝
///
/// 直接在内核完成 file_fd → socket_fd 传输，无用户态内存拷贝。
/// 返回实际传输字节数。
#[cfg(target_os = "linux")]
pub async fn sendfile_to_socket(
    file: &tokio::fs::File,
    socket: &tokio::net::TcpStream,
    offset: u64,
    count: usize,
) -> IoResult<usize> {
    use std::os::unix::io::AsRawFd;
    use tokio::io::Interest;

    let file_fd  = file.as_raw_fd();
    let sock_fd  = socket.as_raw_fd();
    let mut off  = offset as libc::off_t;
    let mut sent = 0usize;
    let mut rem  = count;

    while rem > 0 {
        // 等 socket 可写
        socket.ready(Interest::WRITABLE).await?;

        let n = unsafe {
            libc::sendfile(sock_fd, file_fd, &mut off, rem.min(1 << 21))
        };

        match n {
            n if n > 0 => {
                sent += n as usize;
                rem  -= n as usize;
            }
            0 => break,
            _ => {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock {
                    continue; // EAGAIN，等下一次 writable
                }
                return Err(err);
            }
        }
    }

    Ok(sent)
}

#[cfg(target_os = "macos")]
pub async fn sendfile_to_socket(
    file: &tokio::fs::File,
    socket: &tokio::net::TcpStream,
    offset: u64,
    count: usize,
) -> IoResult<usize> {
    use std::os::unix::io::AsRawFd;
    use tokio::io::Interest;

    let file_fd = file.as_raw_fd();
    let sock_fd = socket.as_raw_fd();
    let mut off = offset as libc::off_t;
    let mut sent = 0usize;
    let mut rem = count;

    while rem > 0 {
        socket.ready(Interest::WRITABLE).await?;
        let mut len = rem.min(1 << 21) as libc::off_t;
        let ret = unsafe {
            libc::sendfile(file_fd, sock_fd, off, &mut len, std::ptr::null_mut(), 0)
        };
        if len > 0 {
            sent += len as usize;
            off  += len;
            rem  -= len as usize;
        }
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock { continue; }
            return Err(err);
        }
    }

    Ok(sent)
}

/// Windows / 不支持 sendfile 的平台：常规分块读
/// 静态文件流式传输降级路径（无 sendfile）
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn file_stream_fallback(
    file: tokio::fs::File,
) -> impl Stream<Item = IoResult<Bytes>> + Send + 'static {
    ReaderStream::with_capacity(file, STREAM_CHUNK)
}

/// 判断当前平台是否支持 sendfile（HTTP/1.1 零拷贝）
#[inline(always)]
pub const fn has_sendfile() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos"))
}
