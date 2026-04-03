//! 压缩（gzip / brotli）及 pread 流式传输辅助

use std::path::Path;
use std::sync::Arc;

use sweety_web::{
    body::ResponseBody,
    http::WebResponse,
};

use crate::config::model::LocationConfig;

/// pread 分块流式传输：共享 fd + spawn_blocking pread，无 seek，无竞争
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
    let stream = crate::handler::sendfile::pread_stream(fd, offset, len);
    let body = ResponseBody::box_stream(stream);
    let mut resp = WebResponse::new(body);
    super::set_file_headers(resp.headers_mut(), file_path, content_len, etag_val, modified_secs, location);
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

/// Brotli 压缩（async-compression，spawn_blocking 避免阻塞 tokio 线程）
pub(super) async fn brotli_compress(data: &[u8]) -> std::io::Result<bytes::Bytes> {
    use async_compression::tokio::bufread::BrotliEncoder;
    use tokio::io::AsyncReadExt;

    let cursor = std::io::Cursor::new(data.to_vec());
    let reader = tokio::io::BufReader::new(cursor);
    let mut encoder = BrotliEncoder::new(reader);
    let mut out = Vec::with_capacity(data.len() / 3);
    encoder.read_to_end(&mut out).await?;
    Ok(bytes::Bytes::from(out))
}
