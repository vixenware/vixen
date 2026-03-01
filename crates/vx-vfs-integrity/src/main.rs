use camino::Utf8PathBuf;
use eyre::{Result, WrapErr};
use facet::Facet;
use figue as args;
use std::io::SeekFrom;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use vx_io::net::Endpoint;
use vx_store_proto::{
    Blake3Hash, BlobNode, CDC_AVG_CHUNK, CDC_MAX_CHUNK, CDC_MIN_CHUNK, CDC_PARAMS_ID, CasNode,
    ChunkEntry, ChunkHash, ContentHash, DirEntryKind, StoreClient,
    cas_tree::{MerkleTreeFile, build_merkle_tree},
};
use vx_vfsd::protocol::VfsdClient;

#[derive(Facet, Debug)]
struct Args {
    /// PRNG seed
    #[facet(args::named, default = 42)]
    seed: u64,

    /// Number of compile iterations per worker
    #[facet(args::named, default = 100)]
    compiles: usize,

    /// Number of parallel workers
    #[facet(args::named, default = 14)]
    workers: usize,

    /// Number of random reads per worker (in addition to compiles)
    #[facet(args::named, default = 1000)]
    reads: usize,

    /// Maximum read size in bytes (default 4MB)
    #[facet(args::named, default = 4_194_304)]
    max_read_size: usize,

    #[facet(flatten)]
    builtins: args::FigueBuiltins,
}

struct StoredFile {
    rel_path: String,
    size: u64,
    data: Vec<u8>,
}

fn find_toolchain_root() -> Result<std::path::PathBuf> {
    let output = std::process::Command::new("rustup")
        .args(["which", "rustc"])
        .output()
        .wrap_err("running `rustup which rustc`")?;
    if !output.status.success() {
        eyre::bail!(
            "rustup which rustc failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let rustc_path = std::path::PathBuf::from(String::from_utf8(output.stdout)?.trim());
    // rustc is at <toolchain>/bin/rustc, go up two levels
    let toolchain_root = rustc_path
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| {
            eyre::eyre!(
                "cannot determine toolchain root from {}",
                rustc_path.display()
            )
        })?;
    Ok(toolchain_root.to_path_buf())
}

fn walk_toolchain(root: &std::path::Path) -> Result<Vec<StoredFile>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .wrap_err_with(|| format!("reading directory {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;

            if file_type.is_dir() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                // Skip share/ (docs, 49K files not needed for compilation)
                // Skip lib/rustlib/src/ (std source, not needed for normal compilation)
                if name_str == "share" && path.parent() == Some(root) {
                    continue;
                }
                if name_str == "src"
                    && path.parent().map(|p| p.ends_with("lib/rustlib")) == Some(true)
                {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file() {
                let rel_path = path
                    .strip_prefix(root)
                    .wrap_err("strip_prefix")?
                    .to_string_lossy()
                    .to_string();
                let data =
                    std::fs::read(&path).wrap_err_with(|| format!("reading {}", path.display()))?;
                let size = data.len() as u64;
                files.push(StoredFile {
                    rel_path,
                    size,
                    data,
                });
            }
            // Skip symlinks — the stable toolchain has 0
        }
    }

    files.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(files)
}

async fn store_file_data(
    store: &StoreClient<roam_core::DriverCaller>,
    data: &[u8],
) -> Result<DirEntryKind> {
    let content_hash = ContentHash(Blake3Hash(blake3::hash(data).into()));

    if data.len() <= CDC_MAX_CHUNK as usize {
        let chunk_hash = ChunkHash(content_hash.0);
        store
            .put_chunk(chunk_hash, data.to_vec())
            .await
            .map(|_| ())
            .map_err(|e| eyre::eyre!("put_chunk: {e:?}"))?;

        Ok(DirEntryKind::SmallFile {
            content_hash,
            size: data.len() as u64,
            executable: true,
        })
    } else {
        let chunker =
            fastcdc::v2020::FastCDC::new(data, CDC_MIN_CHUNK, CDC_AVG_CHUNK, CDC_MAX_CHUNK);

        let mut chunk_entries = Vec::new();
        for chunk in chunker {
            let chunk_data = &data[chunk.offset..chunk.offset + chunk.length];
            let hash = ChunkHash(Blake3Hash(blake3::hash(chunk_data).into()));
            store
                .put_chunk(hash, chunk_data.to_vec())
                .await
                .map(|_| ())
                .map_err(|e| eyre::eyre!("put_chunk: {e:?}"))?;
            chunk_entries.push(ChunkEntry {
                hash,
                size: chunk.length as u32,
            });
        }

        let blob_node = CasNode::Blob(BlobNode {
            chunks: chunk_entries,
            total_size: data.len() as u64,
            content_hash,
            cdc_params_id: CDC_PARAMS_ID,
        });
        let (node_bytes, node_hash) = blob_node.to_bytes_and_hash();
        store
            .put_node(node_hash, node_bytes)
            .await
            .map(|_| ())
            .map_err(|e| eyre::eyre!("put_node blob: {e:?}"))?;

        Ok(DirEntryKind::LargeFile {
            content_hash,
            blob_node: node_hash,
            size: data.len() as u64,
            executable: true,
        })
    }
}

async fn wait_for_socket(endpoint: &Endpoint) -> Result<()> {
    let backoff_ms = [10, 50, 100, 250, 500, 1000, 1500, 2000, 3000, 5000];
    for (attempt, delay) in backoff_ms.iter().enumerate() {
        tokio::time::sleep(tokio::time::Duration::from_millis(*delay)).await;
        if vx_io::net::try_connect(endpoint).await?.is_some() {
            tracing::info!("connected after {} attempts", attempt + 1);
            return Ok(());
        }
    }
    eyre::bail!("timed out waiting for {endpoint}")
}

fn main() -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<()> {
    vx_tracing::init_service("vx_vfs_integrity=debug");

    let args: Args = args::Driver::new(
        args::builder::<Args>()?
            .cli(|cli| cli.args(std::env::args().skip(1)))
            .help(|h| {
                h.program_name("vx-vfs-integrity")
                    .version(env!("CARGO_PKG_VERSION"))
            })
            .build(),
    )
    .run()
    .unwrap();

    let base = Utf8PathBuf::from("/tmp/vx-vfs-integrity");

    // Wipe previous run (retry if needed, as filesystem may be settling)
    for attempt in 0..10 {
        if !tokio::fs::try_exists(&base).await? {
            break;
        }
        match tokio::fs::remove_dir_all(&base).await {
            Ok(_) => break,
            Err(e) if attempt < 9 => {
                tracing::warn!(
                    attempt = attempt + 1,
                    error = %e,
                    "failed to remove directory, retrying in 1s"
                );
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(e) => return Err(e.into()),
        }
    }

    let store_root = base.join("store");
    let mount_point = base.join("mnt");
    let store_socket = base.join("store.sock");
    let vfsd_socket = base.join("vfsd.sock");

    tokio::fs::create_dir_all(&store_root).await?;
    tokio::fs::create_dir_all(&mount_point).await?;

    tracing::info!(%base, "working directory");

    // Find and walk toolchain
    let toolchain_root = find_toolchain_root()?;
    tracing::info!(toolchain_root = %toolchain_root.display(), "found toolchain");

    let files = walk_toolchain(&toolchain_root)?;
    let total_bytes: u64 = files.iter().map(|f| f.size).sum();
    tracing::info!(
        file_count = files.len(),
        total_mb = total_bytes / (1024 * 1024),
        "walked toolchain ({} files, {} MB)",
        files.len(),
        total_bytes / (1024 * 1024)
    );

    // Collect large files for random reads (> 1MB)
    let large_files: Vec<(String, Vec<u8>)> = files
        .iter()
        .filter(|f| f.size > 1024 * 1024)
        .map(|f| (f.rel_path.clone(), f.data.clone()))
        .collect();
    tracing::info!(
        large_file_count = large_files.len(),
        "large files for random reads"
    );

    // Find sibling binaries (same target dir as us)
    let self_exe = std::env::current_exe()?;
    let bin_dir = self_exe.parent().expect("exe has parent dir");

    // Spawn vx-store
    let store_endpoint_str = format!("unix://{store_socket}");
    {
        let mut cmd = std::process::Command::new(bin_dir.join("vx-store"));
        cmd.args([
            "--listen",
            &store_endpoint_str,
            "--root",
            store_root.as_str(),
            "--vx-home",
            base.as_str(),
        ]);
        cmd.stdout(std::process::Stdio::inherit());
        cmd.stderr(std::process::Stdio::inherit());
        let _child =
            ur_taking_me_with_you::spawn_dying_with_parent(cmd).wrap_err("spawning vx-store")?;
        tracing::info!("spawned vx-store");
    }

    let store_endpoint = Endpoint::parse(&store_endpoint_str)?;
    wait_for_socket(&store_endpoint).await?;

    // Spawn vx-vfsd
    let vfsd_endpoint_str = format!("unix://{vfsd_socket}");
    {
        let mut cmd = std::process::Command::new(bin_dir.join("vx-vfsd"));
        cmd.args([
            "--listen",
            &vfsd_endpoint_str,
            "--mount",
            mount_point.as_str(),
            "--store",
            &store_endpoint_str,
        ]);
        cmd.stdout(std::process::Stdio::inherit());
        cmd.stderr(std::process::Stdio::inherit());
        let _child =
            ur_taking_me_with_you::spawn_dying_with_parent(cmd).wrap_err("spawning vx-vfsd")?;
        tracing::info!("spawned vx-vfsd");
    }

    let vfsd_endpoint = Endpoint::parse(&vfsd_endpoint_str)?;
    wait_for_socket(&vfsd_endpoint).await?;

    // Connect RPC clients
    let (store_root, store_session) = vx_io::net::connect_session(&store_endpoint).await?;
    let store_virtual = vx_io::boot::boot_and_connect(&store_root, &store_session).await?;
    let store_client = StoreClient::new(store_virtual);

    let (vfsd_root, vfsd_session) = vx_io::net::connect_session(&vfsd_endpoint).await?;
    let vfsd_virtual = vx_io::boot::boot_and_connect(&vfsd_root, &vfsd_session).await?;
    let vfsd_client = VfsdClient::new(vfsd_virtual);

    // Wait for vfsd to be ready
    vfsd_client
        .ping()
        .await
        .map(|_| ())
        .map_err(|e| eyre::eyre!("vfsd ping: {e:?}"))?;
    tracing::info!("vfsd ready");

    // Store all files and build merkle tree
    tracing::info!("storing {} files in CAS...", files.len());
    let mut tree_files = Vec::with_capacity(files.len());
    let mut stored_bytes = 0u64;

    for (i, file) in files.iter().enumerate() {
        let kind = store_file_data(&store_client, &file.data).await?;
        tree_files.push(MerkleTreeFile {
            path: file.rel_path.clone(),
            kind,
        });
        stored_bytes += file.size;

        if (i + 1) % 100 == 0 || i + 1 == files.len() {
            tracing::info!(
                "{}/{} files stored ({} MB)",
                i + 1,
                files.len(),
                stored_bytes / (1024 * 1024)
            );
        }
    }

    let (root_hash, _total_size) = build_merkle_tree(&store_client, tree_files)
        .await
        .map_err(|e| eyre::eyre!("build_merkle_tree: {e}"))?;
    tracing::info!(%root_hash, "stored directory tree");

    // Mount via vfsd
    let prefix = "tc";
    vfsd_client
        .mkdir(format!("/{prefix}"))
        .await
        .map(|_| ())
        .map_err(|e| eyre::eyre!("mkdir: {e:?}"))?;
    vfsd_client
        .mount(format!("/{prefix}"), root_hash.0)
        .await
        .map(|_| ())
        .map_err(|e| eyre::eyre!("mount: {e:?}"))?;
    tracing::info!("mounted at /{prefix}");

    let tc_mount = mount_point.join(prefix);
    let rustc_path = tc_mount.join("bin/rustc");
    let lib_path = tc_mount.join("lib");

    // Write a small .rs file for compilation
    let source_file = base.join("hello.rs");
    tokio::fs::write(
        &source_file,
        "fn main() { println!(\"hello from vfs-mounted rustc\"); }\n",
    )
    .await?;

    // Verify basic compilation works before spawning workers
    tracing::info!("verifying rustc works from VFS mount...");
    {
        let output = std::process::Command::new(rustc_path.as_std_path())
            .args(["--edition", "2021", source_file.as_str(), "-o"])
            .arg(base.join("hello_verify").as_str())
            .env("DYLD_LIBRARY_PATH", lib_path.as_std_path())
            .env("LD_LIBRARY_PATH", lib_path.as_std_path())
            .output()
            .wrap_err("running rustc from VFS mount")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eyre::bail!("rustc verification failed: {stderr}");
        }
        tracing::info!("rustc verification OK");
    }

    // Build list of VFS paths for large files (for random reads)
    let large_file_info: Vec<(std::path::PathBuf, Vec<u8>)> = large_files
        .into_iter()
        .map(|(rel_path, data)| {
            let vfs_path = tc_mount.join(&rel_path).into_std_path_buf();
            (vfs_path, data)
        })
        .collect();

    // Shared atomic counters for periodic reporting
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

    struct Stats {
        compile_ok: AtomicU64,
        compile_fail: AtomicU64,
        compile_time_ms: AtomicU64,
        read_ok: AtomicU64,
        read_corrupt: AtomicU64,
        read_bytes: AtomicU64,
        workers_done: AtomicU64,
    }

    let stats = std::sync::Arc::new(Stats {
        compile_ok: AtomicU64::new(0),
        compile_fail: AtomicU64::new(0),
        compile_time_ms: AtomicU64::new(0),
        read_ok: AtomicU64::new(0),
        read_corrupt: AtomicU64::new(0),
        read_bytes: AtomicU64::new(0),
        workers_done: AtomicU64::new(0),
    });

    let total_compiles = (args.workers * args.compiles) as u64;
    let total_reads_expected = (args.workers * args.reads) as u64;

    // Shared state for workers
    let rustc_path = std::sync::Arc::new(rustc_path.into_std_path_buf());
    let lib_path = std::sync::Arc::new(lib_path.into_std_path_buf());
    let source_path = std::sync::Arc::new(source_file.into_std_path_buf());
    let output_base = std::sync::Arc::new(base.into_std_path_buf());
    let large_file_info = std::sync::Arc::new(large_file_info);
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(args.workers));

    tracing::info!(
        workers = args.workers,
        compiles_per_worker = args.compiles,
        reads_per_worker = args.reads,
        large_files = large_file_info.len(),
        "starting parallel verification (compile + random reads)"
    );

    // Reporter task: print stats every 2 seconds
    let reporter_stats = stats.clone();
    let reporter_workers = args.workers as u64;
    let reporter = tokio::spawn(async move {
        let start = std::time::Instant::now();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let done = reporter_stats.workers_done.load(Relaxed);
            if done >= reporter_workers {
                break;
            }
            let c_ok = reporter_stats.compile_ok.load(Relaxed);
            let c_fail = reporter_stats.compile_fail.load(Relaxed);
            let c_ms = reporter_stats.compile_time_ms.load(Relaxed);
            let r_ok = reporter_stats.read_ok.load(Relaxed);
            let r_corrupt = reporter_stats.read_corrupt.load(Relaxed);
            let r_bytes = reporter_stats.read_bytes.load(Relaxed);
            let elapsed = start.elapsed().as_secs_f64();
            let c_total = c_ok + c_fail;
            let avg_compile_ms = if c_total > 0 { c_ms / c_total } else { 0 };
            tracing::info!(
                elapsed_s = format!("{elapsed:.1}"),
                compiles =
                    format!("{c_ok}/{total_compiles} ok, {c_fail} fail, avg {avg_compile_ms}ms"),
                reads = format!(
                    "{r_ok}/{total_reads_expected} ok, {r_corrupt} corrupt, {:.1} MB",
                    r_bytes as f64 / (1024.0 * 1024.0)
                ),
                "progress"
            );
        }
    });

    let mut worker_handles = Vec::with_capacity(args.workers);
    for worker_id in 0..args.workers {
        let rustc_path = rustc_path.clone();
        let lib_path = lib_path.clone();
        let source_path = source_path.clone();
        let output_base = output_base.clone();
        let large_file_info = large_file_info.clone();
        let barrier = barrier.clone();
        let stats = stats.clone();
        let compiles = args.compiles;
        let reads = args.reads;
        let max_read_size = args.max_read_size;
        let seed = args.seed;

        worker_handles.push(tokio::spawn(async move {
            let mut rng = fastrand::Rng::with_seed(seed);

            barrier.wait().await;

            let mut compile_failures = 0u64;
            let mut compile_runs = 0u64;
            let mut read_corruptions = 0u64;
            let mut read_count = 0u64;
            let mut read_bytes = 0u64;

            // Phase 1: Compilations
            for i in 0..compiles {
                compile_runs += 1;
                let output_path = output_base.join(format!("out_w{worker_id}_{i}"));
                let mut cmd = tokio::process::Command::new(rustc_path.as_ref());
                cmd.args(["--edition", "2021"])
                    .arg(source_path.as_ref())
                    .arg("-o")
                    .arg(&output_path)
                    .env("DYLD_LIBRARY_PATH", lib_path.as_ref())
                    .env("LD_LIBRARY_PATH", lib_path.as_ref())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());

                let t0 = std::time::Instant::now();
                match cmd.output().await {
                    Ok(output) => {
                        let elapsed_ms = t0.elapsed().as_millis() as u64;
                        stats.compile_time_ms.fetch_add(elapsed_ms, Relaxed);
                        if !output.status.success() {
                            let code = output.status.code().unwrap_or(-1);
                            let stderr = String::from_utf8_lossy(&output.stderr);
                            eprintln!(
                                "COMPILE FAILURE worker {worker_id} iter {i}: \
                                 exit code {code}, stderr: {stderr}"
                            );
                            compile_failures += 1;
                            stats.compile_fail.fetch_add(1, Relaxed);
                        } else {
                            stats.compile_ok.fetch_add(1, Relaxed);
                            let _ = tokio::fs::remove_file(&output_path).await;
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "COMPILE ERROR worker {worker_id} iter {i}: {e} (kind={:?})",
                            e.kind()
                        );
                        compile_failures += 1;
                        stats.compile_fail.fetch_add(1, Relaxed);
                    }
                }
            }

            // Phase 2: Random reads on large files
            if !large_file_info.is_empty() {
                for _ in 0..reads {
                    let file_idx = rng.usize(0..large_file_info.len());
                    let (ref vfs_path, ref expected) = large_file_info[file_idx];
                    let file_size = expected.len();
                    if file_size == 0 {
                        continue;
                    }

                    let offset = rng.usize(0..file_size);
                    let remaining = file_size - offset;
                    let len = rng.usize(1..=remaining.min(max_read_size));

                    let mut file = match tokio::fs::File::open(vfs_path).await {
                        Ok(f) => f,
                        Err(e) => {
                            eprintln!(
                                "READ OPEN ERROR worker {worker_id}: {}: {e}",
                                vfs_path.display()
                            );
                            read_corruptions += 1;
                            stats.read_corrupt.fetch_add(1, Relaxed);
                            continue;
                        }
                    };
                    if let Err(e) = file.seek(SeekFrom::Start(offset as u64)).await {
                        eprintln!("READ SEEK ERROR worker {worker_id}: {e}");
                        read_corruptions += 1;
                        stats.read_corrupt.fetch_add(1, Relaxed);
                        continue;
                    }
                    let mut buf = vec![0u8; len];
                    if let Err(e) = file.read_exact(&mut buf).await {
                        eprintln!(
                            "READ ERROR worker {worker_id}: read_exact({len} bytes at offset {offset}): {e}"
                        );
                        read_corruptions += 1;
                        stats.read_corrupt.fetch_add(1, Relaxed);
                        continue;
                    }

                    read_count += 1;
                    read_bytes += len as u64;
                    stats.read_ok.fetch_add(1, Relaxed);
                    stats.read_bytes.fetch_add(len as u64, Relaxed);

                    let expected_slice = &expected[offset..offset + len];
                    if buf != expected_slice {
                        let mismatch = buf
                            .iter()
                            .zip(expected_slice)
                            .position(|(a, b)| a != b)
                            .unwrap_or(0);
                        let abs_offset = offset + mismatch;
                        eprintln!(
                            "CORRUPTION worker {worker_id} file={} offset={offset} len={len}, \
                             mismatch at file offset {abs_offset}: \
                             expected 0x{:02X} got 0x{:02X}",
                            vfs_path.display(),
                            expected_slice[mismatch],
                            buf[mismatch]
                        );

                        let ctx_start = mismatch.saturating_sub(8);
                        let ctx_end = (mismatch + 8).min(len);
                        eprint!("  expected: ");
                        for b in &expected_slice[ctx_start..ctx_end] {
                            eprint!("{b:02X} ");
                        }
                        eprintln!();
                        eprint!("  actual:   ");
                        for b in &buf[ctx_start..ctx_end] {
                            eprint!("{b:02X} ");
                        }
                        eprintln!();

                        read_corruptions += 1;
                        stats.read_corrupt.fetch_add(1, Relaxed);
                    }
                }
            }

            stats.workers_done.fetch_add(1, Relaxed);

            (compile_failures, compile_runs, read_corruptions, read_count, read_bytes)
        }));
    }

    let mut total_compile_failures = 0u64;
    let mut total_compile_runs = 0u64;
    let mut total_read_corruptions = 0u64;
    let mut total_reads = 0u64;
    let mut total_bytes_read = 0u64;

    for handle in worker_handles {
        let (cf, cr, rc, r, b) = handle.await?;
        total_compile_failures += cf;
        total_compile_runs += cr;
        total_read_corruptions += rc;
        total_reads += r;
        total_bytes_read += b;
    }

    let _ = reporter.await;

    let failures = total_compile_failures + total_read_corruptions;
    if failures > 0 {
        eprintln!(
            "\n{total_compile_failures} compile failures (out of {total_compile_runs}), \
             {total_read_corruptions} read corruptions (out of {total_reads} reads, {:.1} MB)",
            total_bytes_read as f64 / (1024.0 * 1024.0)
        );
        std::process::exit(1);
    } else {
        tracing::info!(
            total_compile_runs,
            total_reads,
            total_mb_read = total_bytes_read / (1024 * 1024),
            workers = args.workers,
            "all compilations + reads verified OK"
        );
    }

    Ok(())
}
