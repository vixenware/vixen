use super::{Compression, ExtractedFile, extract_to_tree};
use flate2::Compression as GzipCompression;
use flate2::write::GzEncoder;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tar::{Builder, Header};
use tokio::io::AsyncReadExt;
use vx_store_proto::chunking::prepare_blob;

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

#[tokio::test]
async fn extract_to_tree_runs_file_callbacks_concurrently() {
    let archive = tar_gz(&[("pkg/a.txt", b"alpha"), ("pkg/b.txt", b"beta")]);
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));

    let (tree, extracted_files) =
        extract_to_tree(std::io::Cursor::new(archive), Compression::Gzip, 1, {
            let active = Arc::clone(&active);
            let max_active = Arc::clone(&max_active);
            move |mut reader, _executable| {
                let active = Arc::clone(&active);
                let max_active = Arc::clone(&max_active);
                Box::pin(async move {
                    let mut data = Vec::new();
                    reader
                        .read_to_end(&mut data)
                        .await
                        .expect("read file contents");
                    let prepared = prepare_blob(&data);
                    let concurrent = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(concurrent, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(ExtractedFile {
                        content_hash: prepared.content_hash,
                        blob_ref: prepared.to_blob_ref(),
                        size: data.len() as u64,
                    })
                })
            }
        })
        .await
        .expect("extract archive");

    let paths = tree
        .entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<Vec<_>>();
    assert_eq!(paths, vec!["a.txt".to_string(), "b.txt".to_string()]);
    assert_eq!(
        extracted_files
            .into_iter()
            .map(|(path, _, _)| path)
            .collect::<Vec<_>>(),
        vec!["a.txt".to_string(), "b.txt".to_string()]
    );
    assert!(
        max_active.load(Ordering::SeqCst) >= 2,
        "expected at least two concurrent file callbacks"
    );
}
