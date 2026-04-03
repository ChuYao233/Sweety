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
use std::sync::Arc;
use bytes::Bytes;
use futures_util::Stream;
use tokio::io::AsyncReadExt;

/// 大文件流式传输块大小（256 KiB）
/// h2 crate 内部按 max_frame_size(16KB) 自动拆帧，这里给大块减少调度开销
pub const STREAM_CHUNK: usize = 256 * 1024;

/// 小文件直接读取阈值（256 KiB）
/// 小于此阈值时一次 read_to_end，避免 spawn + channel 开销
pub const SMALL_FILE_THRESHOLD: u64 = 256 * 1024;

/// mmap 阈值：大于此大小的文件用 mmap 零拷贝传输
/// 小于此就用普通 stream read（避免 mmap 常驻内存的开销）
pub const MMAP_THRESHOLD: u64 = 256 * 1024;

/// 保持 mmap 内存映射活跃的 RAII 包装器
/// Arc 允许多个 Bytes 切片共享同一块 mmap 内存
struct MmapHolder(memmap2::Mmap);
unsafe impl Send for MmapHolder {}
unsafe impl Sync for MmapHolder {}

/// 将文件用 mmap 映射后切成 Bytes 切片流，零内核→用户态拷贝
///
/// # 原理
/// - mmap 后文件数据在 page cache 里，用户态只有指针映射
/// - `Bytes::from_owner` 让 Bytes 直接引用 mmap 内存，没有额外拷贝
/// - H1 write_buf 里的 `Bytes` 描述符指向 page cache，内核 DMA 直接发送
#[allow(dead_code)]
pub fn mmap_file_stream(
    file: std::fs::File,
    file_size: u64,
) -> impl Stream<Item = IoResult<Bytes>> + Send + 'static {
    async_stream::stream! {
        if file_size == 0 { return; }
        let chunk_size = STREAM_CHUNK;
        let total = file_size as usize;
        let mut offset = 0usize;
        while offset < total {
            let end = (offset + chunk_size).min(total);
            let len  = end - offset;
            // 每次只 mmap 当前 chunk（256KB），yield 后 Arc 归零立刻 unmap
            // 内存恒定在 chunk_size × 并发流数，不随文件大小增长
            let mmap = match unsafe {
                memmap2::MmapOptions::new()
                    .offset(offset as u64)
                    .len(len)
                    .map(&file)
            } {
                Ok(m) => Arc::new(MmapHolder(m)),
                Err(e) => { yield Err(e); return; }
            };
            // 告知内核顺序预读本 chunk，减少缺页延迟
            #[cfg(target_os = "linux")]
            let _ = mmap.0.advise(memmap2::Advice::Sequential);
            let bytes = bytes::Bytes::from_owner(MmapSlice {
                _owner: mmap.clone(),
                ptr: mmap.0.as_ptr(),
                len,
            });
            offset = end;
            yield Ok(bytes);
            // bytes drop 后 mmap Arc 引用归零，内核立刻 unmap 这段 256KB
        }
    }
}

/// mmap 切片的 Bytes owner （持有 Arc<MmapHolder> 保证内存不被释放）
struct MmapSlice {
    _owner: Arc<MmapHolder>,
    ptr:    *const u8,
    len:    usize,
}
unsafe impl Send for MmapSlice {}
unsafe impl Sync for MmapSlice {}

impl AsRef<[u8]> for MmapSlice {
    fn as_ref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

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
    // 动态 chunk size：小文件用小块，中/大文件统一 256KB
    // 中等文件（64KB~）用 256KB chunk 对齐 H2 pipeline CHUNK_SIZE，减少 poll_next 次数
    let chunk_size = if len <= 64 * 1024 {
        16 * 1024usize  // 小文件：16KB chunk
    } else {
        STREAM_CHUNK     // 中/大文件：256KB chunk，与 H2 PIPELINE_WINDOW 对齐
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

// ──────────────────────────────────────────────────────────────────────────────
// pread 系列：共享 fd + 无 seek 偏移读
// ──────────────────────────────────────────────────────────────────────────────

/// 异步版：从共享 fd 的 offset 读取 len 字节（一次性），无 spawn_blocking
///
/// try_clone 给出独立 fd，seek + read_exact 全部在 async 上下文完成。
pub async fn async_read_range(
    file: &std::sync::Arc<std::fs::File>,
    offset: u64,
    len: usize,
) -> IoResult<Bytes> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    if len == 0 {
        return Ok(Bytes::new());
    }
    let std_file = file.try_clone()?;
    let mut async_file = tokio::fs::File::from_std(std_file);
    if offset > 0 {
        async_file.seek(std::io::SeekFrom::Start(offset)).await?;
    }
    let mut buf = bytes::BytesMut::with_capacity(len);
    buf.resize(len, 0);
    async_file.read_exact(&mut buf).await?;
    Ok(buf.freeze())
}

/// 从共享 fd 的指定 offset 读取 len 字节（一次性，同步版）
///
/// - Linux/macOS：使用 `pread(2)` 系统调用，无 seek，无竞争，线程安全
/// - Windows：先 dup fd，再同步 seek + read_exact（单线程，无竞争）
///
/// 此函数必须在 `spawn_blocking` 线程池内调用（同步阻塞）。
pub fn pread_exact(
    file: &std::fs::File,
    offset: u64,
    len: usize,
) -> IoResult<Bytes> {
    if len == 0 {
        return Ok(Bytes::new());
    }
    let mut buf = vec![0u8; len];
    pread_all(file, offset, &mut buf)?;
    Ok(Bytes::from(buf))
}

/// 从共享 fd 的 offset 开始，生成一个分块 Stream
///
/// 使用 pread(2) 系统调用：直接在指定 offset 读，无 seek，无 try_clone，共享 fd 线程安全。
/// 按 chunk_size 分块，每块一次 spawn_blocking，高并发下 pread 耗时极短（微秒级），
/// blocking 线程池不会长期被占用。
pub fn pread_stream(
    file: std::sync::Arc<std::fs::File>,
    offset: u64,
    len: u64,
) -> impl Stream<Item = IoResult<Bytes>> + Send + 'static {
    let chunk_size = if len <= 256 * 1024 {
        len as usize  // 小/中文件一次读完，只用一次 spawn_blocking
    } else {
        STREAM_CHUNK  // 大文件分 256KB chunk
    };

    async_stream::stream! {
        let mut off = offset;
        let mut remaining = len;

        while remaining > 0 {
            let to_read = (remaining as usize).min(chunk_size);
            let f = file.clone();
            match tokio::task::spawn_blocking(move || pread_exact(&f, off, to_read)).await {
                Ok(Ok(bytes)) => {
                    let n = bytes.len() as u64;
                    if n == 0 { break; }
                    off += n;
                    remaining = remaining.saturating_sub(n);
                    yield Ok(bytes);
                }
                Ok(Err(e)) => { yield Err(e); return; }
                Err(e) => {
                    yield Err(std::io::Error::new(std::io::ErrorKind::Other, e));
                    return;
                }
            }
        }
    }
}

/// 内部实现：从 fd + offset 连续读满 buf（处理短读）
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn pread_all(file: &std::fs::File, offset: u64, buf: &mut [u8]) -> IoResult<()> {
    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    let mut done = 0usize;
    while done < buf.len() {
        let n = unsafe {
            libc::pread(
                fd,
                buf[done..].as_mut_ptr() as *mut libc::c_void,
                buf.len() - done,
                (offset + done as u64) as libc::off_t,
            )
        };
        match n {
            n if n > 0 => done += n as usize,
            // pread(2) 返回 0 表示已到达文件末尾，正常退出
            0 => break,
            _ => {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted { continue; }
                return Err(e);
            }
        }
    }
    Ok(())
}

/// Windows / 其他平台：dup fd 后同步 seek + read_exact
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn pread_all(file: &std::fs::File, offset: u64, buf: &mut [u8]) -> IoResult<()> {
    use std::io::{Read, Seek, SeekFrom};
    // try_clone() 给出独立的文件指针，不影响原 fd
    let mut dup = file.try_clone()?;
    dup.seek(SeekFrom::Start(offset))?;
    dup.read_exact(buf)
}
