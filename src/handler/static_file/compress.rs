//! 压缩（gzip / brotli / zstd）及 pread 流式传输辅助

use std::path::Path;
use std::sync::Arc;

use sweety_web::{
    body::ResponseBody,
    http::{
        WebResponse,
        header::{CONTENT_ENCODING, HeaderValue},
    },
};

use crate::config::model::LocationConfig;

/// 大文件流式输出时选用的压缩算法
///
/// `None` 表示不压缩，调用方可直接走原有的 `stream_file_response_pread` 零分支开销。
#[derive(Clone, Copy)]
pub(super) enum CompressEncoding {
    Gzip(u32),
    Brotli(u32),
    Zstd(u32),
}

/// 大文件流式压缩响应
///
/// - `enc` 为 `None` 时直接调用 `stream_file_response_pread`，完全零开销。
/// - 有压缩时：`pread_stream` → `StreamReader` → async-compression encoder
///   → `ReaderStream` → `ResponseBody::box_stream`，内容长度未知故不设 Content-Length。
/// - Range 请求、sendfile 路径与此函数互斥，调用方负责不传入 Range 场景。
#[allow(unused_variables)]
pub(super) fn stream_file_response_compressed(
    fd: Arc<std::fs::File>,
    file_path: &Path,
    file_size: u64,
    etag_val: &str,
    modified_secs: u64,
    location: &LocationConfig,
    enc: Option<CompressEncoding>,
) -> WebResponse {
    let Some(enc) = enc else {
        // 无压缩：直接走原有路径，零额外开销
        return stream_file_response_pread(fd, file_path, file_size, 0, file_size, etag_val, modified_secs, location);
    };

    let stream = crate::handler::sendfile::pread_stream(fd, 0, file_size);

    // 用 tokio_util 把 Stream<Item=Bytes> 转换为 AsyncRead
    use tokio_util::io::{StreamReader, ReaderStream};
    use futures_util::TryStreamExt;
    use async_compression::Level;

    // 将 io::Error stream 直接用（sendfile stream 已是 io::Result<Bytes>）
    let reader = StreamReader::new(stream);

    // 按算法分沐：仅编译启用的分支的代码存在，未用的分支由编译器内联/死代码消除
    let (body, enc_name): (ResponseBody, &'static str) = match enc {
        CompressEncoding::Gzip(level) => {
            use async_compression::tokio::bufread::GzipEncoder;
            let encoder = GzipEncoder::with_quality(reader, Level::Precise(level.min(9) as i32));
            let out_stream = ReaderStream::new(encoder).map_err(|e| {
                tracing::warn!("流式 gzip 压缩错误: {}", e);
                e
            });
            (ResponseBody::box_stream(out_stream), "gzip")
        }
        CompressEncoding::Brotli(level) => {
            use async_compression::tokio::bufread::BrotliEncoder;
            let encoder = BrotliEncoder::with_quality(reader, Level::Precise(level.min(11) as i32));
            let out_stream = ReaderStream::new(encoder).map_err(|e| {
                tracing::warn!("流式 brotli 压缩错误: {}", e);
                e
            });
            (ResponseBody::box_stream(out_stream), "br")
        }
        CompressEncoding::Zstd(level) => {
            use async_compression::tokio::bufread::ZstdEncoder;
            let encoder = ZstdEncoder::with_quality(reader, Level::Precise(level.clamp(1, 22) as i32));
            let out_stream = ReaderStream::new(encoder).map_err(|e| {
                tracing::warn!("流式 zstd 压缩错误: {}", e);
                e
            });
            (ResponseBody::box_stream(out_stream), "zstd")
        }
    };

    let mut resp = WebResponse::new(body);
    // 压缩流内容长度未知，不设 Content-Length（HTTP/1.1 用 chunked，H2/H3 不需要）
    super::set_file_headers_no_length(resp.headers_mut(), file_path, etag_val, modified_secs, location);
    if let Ok(v) = HeaderValue::from_str(enc_name) {
        resp.headers_mut().insert(CONTENT_ENCODING, v);
    }
    // RFC 7231 §7.1.4：压缩响应必须携带 Vary: Accept-Encoding
    resp.headers_mut().insert(
        sweety_web::http::header::VARY,
        HeaderValue::from_static("Accept-Encoding"),
    );
    resp
}

/// pread 分块流式传输：共享 fd + 异步 seek+read，无竞争
///
/// 同时向 response extensions 注入 `SendFileInfo`，H1 非 TLS 路径会走 sendfile(2) 零拷贝；
/// TLS / H2 / H3 忽略 extension，使用 body 里的 pread_stream 作为回退。
pub(super) fn stream_file_response_pread(
    fd: Arc<std::fs::File>,
    file_path: &Path,
    content_len: u64,
    offset: u64,
    len: u64,
    etag_val: &str,
    modified_secs: u64,
    location: &LocationConfig,
) -> WebResponse {
    let stream = crate::handler::sendfile::pread_stream(fd.clone(), offset, len);
    let body = ResponseBody::box_stream(stream);
    let mut resp = WebResponse::new(body);
    super::set_file_headers(resp.headers_mut(), file_path, content_len, etag_val, modified_secs, location);

    // Linux H1 非 TLS：注入 SendFileInfo，dispatcher 检测后走 sendfile(2) 零拷贝
    #[cfg(target_os = "linux")]
    resp.extensions_mut().insert(
        sweety_web::SendFileInfo { fd, offset, len }
    );

    resp
}

/// gzip 压缩（flate2，仅用于小文件；调用方需通过 spawn_blocking 调用）
pub(super) fn gzip_compress(data: &[u8], level: u32) -> std::io::Result<bytes::Bytes> {
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;
    let mut encoder = GzEncoder::new(
        Vec::with_capacity(data.len() / 2),
        Compression::new(level.min(9)),
    );
    encoder.write_all(data)?;
    Ok(bytes::Bytes::from(encoder.finish()?))
}

/// Brotli 压缩（async-compression，level 0-11，默认 4）
pub(super) async fn brotli_compress(data: &[u8], level: u32) -> std::io::Result<bytes::Bytes> {
    use async_compression::tokio::bufread::BrotliEncoder;
    use async_compression::Level;
    use tokio::io::AsyncReadExt;

    let cursor = std::io::Cursor::new(data.to_vec());
    let reader = tokio::io::BufReader::new(cursor);
    let mut encoder = BrotliEncoder::with_quality(reader, Level::Precise(level.min(11) as i32));
    let mut out = Vec::with_capacity(data.len() / 3);
    encoder.read_to_end(&mut out).await?;
    Ok(bytes::Bytes::from(out))
}

/// zstd 压缩（async-compression，level 1-22，默认 3）
pub(super) async fn zstd_compress(data: &[u8], level: u32) -> std::io::Result<bytes::Bytes> {
    use async_compression::tokio::bufread::ZstdEncoder;
    use async_compression::Level;
    use tokio::io::AsyncReadExt;

    let cursor = std::io::Cursor::new(data.to_vec());
    let reader = tokio::io::BufReader::new(cursor);
    let mut encoder = ZstdEncoder::with_quality(reader, Level::Precise(level.clamp(1, 22) as i32));
    let mut out = Vec::with_capacity(data.len() / 3);
    encoder.read_to_end(&mut out).await?;
    Ok(bytes::Bytes::from(out))
}
