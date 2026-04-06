//! 压缩（gzip / brotli / zstd）及 pread 流式传输辅助

use std::path::Path;
use std::sync::Arc;

use sweety_web::{
    body::ResponseBody,
    http::WebResponse,
};

use crate::config::model::LocationConfig;

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
