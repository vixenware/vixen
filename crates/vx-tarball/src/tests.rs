use super::{Compression, ExtractedFile};
use flate2::Compression as GzipCompression;
use flate2::write::GzEncoder;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tar::{Builder, Header};
use tokio::io::AsyncReadExt;
use vx_services::chunking::prepare_blob;

fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut archive_bytes = Vec::new();
    {
        let encoder = GzEncoder::new(&mut archive_bytes, GzipCompression::default());
        let mut builder = Builder::new(encoder);
        for (path, contents) in files {
            let mut header = Header::new_gnu();
            header.set_mode(0o644);
            header.set_size(contents.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, path, &contents[..])
                .expect("append file");
        }
        builder.finish().expect("finish tar builder");
        builder
            .into_inner()
            .expect("into_inner")
            .finish()
            .expect("finish gzip encoder");
    }
    archive_bytes
}
