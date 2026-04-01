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

/// 大文件流式传输块大小（1 MiB）
/// 提升 HTTPS/H2 大文件吸吐：更大的块减少 channel 轮次，降低调度开销
pub const STREAM_CHUNK: usize = 1024 * 1024;

/// 小文件直接读取阈値（256 KiB）
/// 小于此阈値时一次 read_to_end，避免 spawn + channel 开销
pub const SMALL_FILE_THRESHOLD: u64 = 256 * 1024;

/// 返回一个带背压的文件 stream（无 spawn、直接在调用任务里读取）
///
/// 不再歡用 tokio::spawn，而是用 async_stream 宏直接在 handler task 里展开。
/// 消除 500 并发请求 = 500 额外 task 的调度开销，降低 P99 尾延迟抖动。
pub fn file_stream_backpressure<R>(
    reader: R,
    len: u64,
) -> impl Stream<Item = IoResult<Bytes>> + Send + 'static
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    // 动态 chunk size：小文件用小块，大文件用 1MB
    // 避免 1KB 文件分配 1MB buf
    let chunk_size = if len <= 64 * 1024 {
        16 * 1024usize  // 小文件：16KB chunk
    } else if len <= 1024 * 1024 {
        128 * 1024      // 中文件：128KB chunk
    } else {
        STREAM_CHUNK     // 大文件：1MB chunk
    };

    async_stream::stream! {
        let mut reader = reader;
        let mut remaining = len;
        let mut buf = bytes::BytesMut::with_capacity(chunk_size);

        while remaining > 0 {
            let to_read = (remaining as usize).min(chunk_size);
            buf.resize(to_read, 0);

            match reader.read(&mut buf[..to_read]).await {
                Ok(0) => break,
                Ok(n) => {
                    remaining = remaining.saturating_sub(n as u64);
                    yield Ok(buf.split_to(n).freeze());
                }
                Err(e) => {
                    yield Err(e);
                    break;
                }
            }
        }
    }
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

/// Windows / 不支持 sendfile 的平台：常规分块读（复用背压 stream）
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn file_stream_fallback(
    file: tokio::fs::File,
    len: u64,
) -> impl Stream<Item = IoResult<Bytes>> + Send + 'static {
    file_stream_backpressure(file, len)
}

/// 判断当前平台是否支持 sendfile（HTTP/1.1 零拷贝）
#[inline(always)]
pub const fn has_sendfile() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos"))
}
