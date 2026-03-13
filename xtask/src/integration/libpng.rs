use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use eyre::{Result, bail};
use flate2::Compression as GzipCompression;
use flate2::write::GzEncoder;
use sha2::{Digest as _, Sha256};
use tar::Builder as TarBuilder;

use super::{
    ScenarioFuture,
    context::{BuildOptions, IntegrationContext},
};

const LIBPNG_SKETCH: &str = include_str!("../../../zoo/libpng.vx");

pub(crate) fn run(context: &mut IntegrationContext) -> ScenarioFuture<'_> {
    Box::pin(async move {
        #[cfg(not(unix))]
        bail!("the libpng integration scenario currently requires a unix-like host");

        #[cfg(unix)]
        {
            let fake_tools = tempfile::tempdir()?;
            write_executable_script(
                &fake_tools.path().join("cc"),
                r#"#!/bin/sh
if [ "$1" = "-dumpmachine" ]; then
  echo x86_64-unknown-linux-gnu
  exit 0
fi
out=""
prev=""
for arg in "$@"; do
  if [ "$prev" = "-o" ]; then
    out="$arg"
    break
  fi
  prev="$arg"
done
if [ -z "$out" ]; then
  echo "missing -o output" >&2
  exit 1
fi
mkdir -p "$(dirname "$out")"
printf 'artifact\n' > "$out"
exit 0
"#,
            )?;
            write_executable_script(
                &fake_tools.path().join("ar"),
                r#"#!/bin/sh
out="$2"
if [ -z "$out" ]; then
  echo "missing archive output" >&2
  exit 1
fi
mkdir -p "$(dirname "$out")"
printf 'archive\n' > "$out"
exit 0
"#,
            )?;
            context.prepend_path(fake_tools.path())?;

            let zlib_archive = tar_gz_with_single_root(&[
                ("zlib-1.3.1/adler32.c", b""),
                ("zlib-1.3.1/compress.c", b""),
                ("zlib-1.3.1/crc32.c", b""),
                ("zlib-1.3.1/deflate.c", b""),
                ("zlib-1.3.1/gzclose.c", b""),
                ("zlib-1.3.1/gzlib.c", b""),
                ("zlib-1.3.1/gzread.c", b""),
                ("zlib-1.3.1/gzwrite.c", b""),
                ("zlib-1.3.1/inflate.c", b""),
                ("zlib-1.3.1/infback.c", b""),
                ("zlib-1.3.1/inftrees.c", b""),
                ("zlib-1.3.1/inffast.c", b""),
                ("zlib-1.3.1/trees.c", b""),
                ("zlib-1.3.1/uncompr.c", b""),
                ("zlib-1.3.1/zutil.c", b""),
                ("zlib-1.3.1/zlib.h", b"/* stub zlib.h */\n"),
                ("zlib-1.3.1/zconf.h", b"/* stub zconf.h */\n"),
            ]);
            let libpng_archive = tar_gz_with_single_root(&[
                ("libpng-1.6.43/png.c", b""),
                ("libpng-1.6.43/pngerror.c", b""),
                ("libpng-1.6.43/pngget.c", b""),
                ("libpng-1.6.43/pngmem.c", b""),
                ("libpng-1.6.43/pngpread.c", b""),
                ("libpng-1.6.43/pngread.c", b""),
                ("libpng-1.6.43/pngrio.c", b""),
                ("libpng-1.6.43/pngrtran.c", b""),
                ("libpng-1.6.43/pngrutil.c", b""),
                ("libpng-1.6.43/pngset.c", b""),
                ("libpng-1.6.43/pngtrans.c", b""),
                ("libpng-1.6.43/pngwio.c", b""),
                ("libpng-1.6.43/pngwrite.c", b""),
                ("libpng-1.6.43/pngwtran.c", b""),
                ("libpng-1.6.43/pngwutil.c", b""),
                ("libpng-1.6.43/arm/arm_init.c", b""),
                ("libpng-1.6.43/arm/filter_neon_intrinsics.c", b""),
                ("libpng-1.6.43/arm/palette_neon_intrinsics.c", b""),
                ("libpng-1.6.43/arm/filter_neon.S", b""),
                ("libpng-1.6.43/intel/intel_init.c", b""),
                ("libpng-1.6.43/intel/filter_sse2_intrinsics.c", b""),
                ("libpng-1.6.43/png.h", b"/* stub png.h */\n"),
                ("libpng-1.6.43/pngconf.h", b"/* stub pngconf.h */\n"),
                (
                    "libpng-1.6.43/scripts/pnglibconf.h.prebuilt",
                    b"/* stub pnglibconf.h */\n",
                ),
                ("libpng-1.6.43/libpng.vers", b"{ global: *; };\n"),
            ]);

            let zlib_digest = format!("sha256:{}", hex::encode(Sha256::digest(&zlib_archive)));
            let libpng_digest = format!("sha256:{}", hex::encode(Sha256::digest(&libpng_archive)));
            let zlib_server = StaticHttpServer::new(zlib_archive, "application/gzip");
            let libpng_server = StaticHttpServer::new(libpng_archive, "application/gzip");

            let build_vx = LIBPNG_SKETCH
                .replace(
                    "https://zlib.net/fossils/zlib-1.3.1.tar.gz",
                    &zlib_server.url("/zlib-1.3.1.tar.gz"),
                )
                .replace(
                    "sha256:9a93b2b7dfdac77ceba5a558a580e74667dd6fede4585b91eefb60f03b72df23",
                    &zlib_digest,
                )
                .replace(
                    "https://download.sourceforge.net/libpng/libpng-1.6.43.tar.xz",
                    &libpng_server.url("/libpng-1.6.43.tar.gz"),
                )
                .replace(
                    "sha256:6a5ca0652392a2d7c9db2ae5b40210843c0bbc081cbd410825ab00cc59f14a6c",
                    &libpng_digest,
                )
                .replace("extract: \"tar.xz\"", "extract: \"tar.gz\"");
            context.write_project_file("build.vx", &build_vx)?;

            let first = context.run_vx_build(BuildOptions::default()).await?;
            first.assert_success("first vx build")?;

            let second = context.run_vx_build(BuildOptions::default()).await?;
            second.assert_success("second vx build")?;

            if zlib_server.hit_count() != 1 {
                bail!(
                    "expected exactly one zlib acquire fetch, got {}",
                    zlib_server.hit_count()
                );
            }
            if libpng_server.hit_count() != 1 {
                bail!(
                    "expected exactly one libpng acquire fetch, got {}",
                    libpng_server.hit_count()
                );
            }

            let actual = collect_fs_paths(&context.project_path(".vx/build"))?;
            let expected = vec![
                "libpng/include/png.h".to_string(),
                "libpng/include/pngconf.h".to_string(),
                "libpng/include/pnglibconf.h".to_string(),
                "libpng/lib/libpng16.a".to_string(),
                "libpng/lib/libpng16.so".to_string(),
                "zlib/include/zconf.h".to_string(),
                "zlib/include/zlib.h".to_string(),
                "zlib/lib/libz.a".to_string(),
                "zlib/lib/libz.so".to_string(),
            ];
            if actual != expected {
                bail!(
                    "unexpected libpng build outputs\nexpected: {expected:#?}\nactual: {actual:#?}"
                );
            }

            Ok(())
        }
    })
}

fn write_executable_script(path: &Path, body: &str) -> Result<()> {
    fs::write(path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = fs::metadata(path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

fn collect_fs_paths(root: &Path) -> Result<Vec<String>> {
    fn walk(root: &Path, current: &Path, out: &mut Vec<String>) -> Result<()> {
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out)?;
            } else {
                out.push(
                    path.strip_prefix(root)
                        .expect("path under root")
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
        Ok(())
    }

    let mut paths = Vec::new();
    walk(root, root, &mut paths)?;
    paths.sort();
    Ok(paths)
}

struct StaticHttpServer {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl StaticHttpServer {
    fn new(body: Vec<u8>, content_type: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind test http server");
        let addr = listener
            .local_addr()
            .expect("failed to read test http server address");
        let hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let hits_for_thread = Arc::clone(&hits);
        let stop_for_thread = Arc::clone(&stop);
        let thread = thread::spawn(move || {
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        if stop_for_thread.load(Ordering::Relaxed) {
                            break;
                        }
                        let mut request = [0u8; 1024];
                        let _ = stream.read(&mut request);
                        hits_for_thread.fetch_add(1, Ordering::Relaxed);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n",
                            body.len(),
                            content_type,
                        );
                        stream
                            .write_all(response.as_bytes())
                            .expect("failed to write response headers");
                        stream
                            .write_all(&body)
                            .expect("failed to write response body");
                    }
                    Err(error) => panic!("test http server accept failed: {error}"),
                }
            }
        });

        Self {
            addr,
            hits,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    fn hit_count(&self) -> usize {
        self.hits.load(Ordering::Relaxed)
    }
}

impl Drop for StaticHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(self.addr);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("test http server thread should join");
        }
    }
}

fn tar_gz_with_single_root(files: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), GzipCompression::default());
    let mut builder = TarBuilder::new(encoder);

    for (path, bytes) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, path, *bytes)
            .expect("failed to append tar entry");
    }

    let encoder = builder.into_inner().expect("failed to finish tar archive");
    encoder.finish().expect("failed to finish gzip stream")
}
