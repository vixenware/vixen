use crate::registry::download_crate;
use crate::types::StoreService;
use moire::task::FutureExt;
use std::time::{Duration, Instant};
use vx_store_proto::Store as StoreRpc;
use vx_store_proto::chunking::{PreparedBlob, prepare_blob};
use vx_store_proto::{
    Blake3Hash, BlobRef, CDC_MAX_CHUNK, CacheKey, CasNode, ChunkHash, DirEntryKind, DirectoryNode,
    EnsureRegistryCrateResult, EnsureRustToolchainComponentResult, EnsureStatus,
    EnsureToolchainResult, KNOWN_CDC_PARAMS, MAX_CHUNKS_PER_BLOB, MAX_MISSING_CHUNKS_BATCH,
    MAX_MISSING_NODES_BATCH, MAX_NODE_BYTES, MaterializationPlan, MaterializeStep, NodeHash,
    PublishResult, REGISTRY_MANIFEST_SCHEMA_VERSION, RegistryCrateManifest, RegistrySpec,
    RegistrySpecKey, RustComponent, RustToolchainSpec, STORE_PROTOCOL_VERSION, ServiceVersion,
    StoreProgress, ToolchainKind, ToolchainManifest, ToolchainSpecKey, ZigToolchainSpec,
};

const CRATES_IO_REGISTRY: &str = "https://crates.io";
const MATERIALIZATION_LAYOUT_VERSION: u32 = 2;
const PROGRESS_SEND_TIMEOUT: Duration = Duration::from_millis(25);
const PROGRESS_BIND_GRACE_PERIOD: Duration = Duration::from_secs(1);
const PROGRESS_BIND_RETRY_INTERVAL: Duration = Duration::from_millis(10);

async fn send_progress_best_effort(progress: &roam::Tx<StoreProgress>, update: StoreProgress) {
    let deadline = Instant::now() + PROGRESS_BIND_GRACE_PERIOD;
    loop {
        match tokio::time::timeout(PROGRESS_SEND_TIMEOUT, progress.send(update.clone())).await {
            Ok(Ok(())) => return,
            Ok(Err(roam::TxError::Unbound)) if Instant::now() < deadline => {
                tokio::time::sleep(PROGRESS_BIND_RETRY_INTERVAL).await;
            }
            _ => return,
        }
    }
}

impl StoreService {
    async fn version(&self) -> ServiceVersion {
        ServiceVersion {
            service: "vx-store".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: STORE_PROTOCOL_VERSION,
        }
    }

    // =========================================================================
    // Chunk operations
    // =========================================================================

    async fn put_chunk(&self, hash: ChunkHash, data: Vec<u8>) -> Result<(), String> {
        if self.present_chunks.lock().contains(&hash) {
            return Ok(());
        }
        if self
            .storage
            .has_chunk(&hash)
            .named("storage.has_chunk")
            .await
            .map_err(|e| format!("storage error: {e}"))?
        {
            self.present_chunks.lock().insert(hash);
            return Ok(());
        }

        // Validate size
        if data.len() > CDC_MAX_CHUNK as usize {
            return Err(format!(
                "chunk too large: {} bytes (max {})",
                data.len(),
                CDC_MAX_CHUNK
            ));
        }

        // Validate hash
        let actual = Blake3Hash::from_bytes(&data);
        if actual != hash.0 {
            return Err(format!(
                "hash mismatch: expected {}, got {}",
                hash.to_hex(),
                actual.to_hex()
            ));
        }

        self.storage
            .put_chunk(&hash, &data)
            .named("storage.put_chunk")
            .await
            .map_err(|e| format!("storage error: {e}"))?;
        self.present_chunks.lock().insert(hash);
        Ok(())
    }

    async fn get_chunk(&self, hash: ChunkHash) -> Option<Vec<u8>> {
        let data = self.storage.get_chunk(&hash).await.ok().flatten();
        if data.is_some() {
            self.present_chunks.lock().insert(hash);
            return data;
        }
        let upstream = self.upstream();
        let data = upstream?.get_chunk(hash).await.ok().flatten()?;
        if StoreService::put_chunk(self, hash, data.clone())
            .await
            .is_ok()
        {
            return Some(data);
        }
        None
    }

    async fn has_chunk(&self, hash: ChunkHash) -> bool {
        if self.present_chunks.lock().contains(&hash) {
            return true;
        }
        let present = self.storage.has_chunk(&hash).await.unwrap_or(false);
        if present {
            self.present_chunks.lock().insert(hash);
            return true;
        }
        let Some(upstream) = self.upstream() else {
            return false;
        };
        let present = upstream.has_chunk(hash).await.unwrap_or(false);
        if present {
            self.present_chunks.lock().insert(hash);
        }
        present
    }

    async fn list_missing_chunks(&self, hashes: Vec<ChunkHash>) -> Vec<ChunkHash> {
        let hashes = if hashes.len() > MAX_MISSING_CHUNKS_BATCH {
            tracing::warn!(
                count = hashes.len(),
                max = MAX_MISSING_CHUNKS_BATCH,
                "list_missing_chunks: batch too large, truncating"
            );
            hashes.into_iter().take(MAX_MISSING_CHUNKS_BATCH).collect()
        } else {
            hashes
        };

        let mut missing = Vec::new();
        for hash in hashes {
            if self.present_chunks.lock().contains(&hash) {
                continue;
            }
            match self.storage.has_chunk(&hash).await {
                Ok(true) => {
                    self.present_chunks.lock().insert(hash);
                }
                _ => {
                    if let Some(upstream) = self.upstream()
                        && upstream.has_chunk(hash).await.unwrap_or(false)
                    {
                        self.present_chunks.lock().insert(hash);
                        continue;
                    }
                    missing.push(hash);
                }
            }
        }
        missing
    }

    // =========================================================================
    // Node operations
    // =========================================================================

    async fn put_node(&self, hash: NodeHash, data: Vec<u8>) -> Result<(), String> {
        if self.present_nodes.lock().contains(&hash) {
            return Ok(());
        }
        if self
            .storage
            .has_node(&hash)
            .named("storage.has_node")
            .await
            .map_err(|e| format!("storage error: {e}"))?
        {
            self.present_nodes.lock().insert(hash);
            return Ok(());
        }

        // Validate size
        if data.len() > MAX_NODE_BYTES {
            return Err(format!(
                "node too large: {} bytes (max {})",
                data.len(),
                MAX_NODE_BYTES
            ));
        }

        // Validate hash (uniform addressing for ALL node types)
        let actual = Blake3Hash::from_bytes(&data);
        if actual != hash.0 {
            return Err(format!(
                "hash mismatch: expected {}, got {}",
                hash.to_hex(),
                actual.to_hex()
            ));
        }

        // Deserialize and validate structure
        let node = CasNode::from_bytes(&data).map_err(|e| format!("invalid node: {e}"))?;

        match &node {
            CasNode::Blob(blob) => validate_blob_node(blob)?,
            CasNode::Directory(dir) => validate_directory_node(dir)?,
            // Other node types: basic structural validation (deserialization already covers it)
            CasNode::Manifest(_) | CasNode::Toolchain(_) | CasNode::RegistryCrate(_) => {}
        }

        self.storage
            .put_node(&hash, &data)
            .await
            .map_err(|e| format!("storage error: {e}"))?;
        self.present_nodes.lock().insert(hash);
        Ok(())
    }

    async fn get_node(&self, hash: NodeHash) -> Option<Vec<u8>> {
        let data = self.storage.get_node(&hash).await.ok().flatten();
        if data.is_some() {
            self.present_nodes.lock().insert(hash);
            return data;
        }
        let upstream = self.upstream();
        let data = upstream?.get_node(hash).await.ok().flatten()?;
        if StoreService::put_node(self, hash, data.clone())
            .await
            .is_ok()
        {
            return Some(data);
        }
        None
    }

    async fn has_node(&self, hash: NodeHash) -> bool {
        if self.present_nodes.lock().contains(&hash) {
            return true;
        }
        let present = self.storage.has_node(&hash).await.unwrap_or(false);
        if present {
            self.present_nodes.lock().insert(hash);
            return true;
        }
        let Some(upstream) = self.upstream() else {
            return false;
        };
        let present = upstream.has_node(hash).await.unwrap_or(false);
        if present {
            self.present_nodes.lock().insert(hash);
        }
        present
    }

    async fn list_missing_nodes(&self, hashes: Vec<NodeHash>) -> Vec<NodeHash> {
        let hashes = if hashes.len() > MAX_MISSING_NODES_BATCH {
            tracing::warn!(
                count = hashes.len(),
                max = MAX_MISSING_NODES_BATCH,
                "list_missing_nodes: batch too large, truncating"
            );
            hashes.into_iter().take(MAX_MISSING_NODES_BATCH).collect()
        } else {
            hashes
        };

        let mut missing = Vec::new();
        for hash in hashes {
            if self.present_nodes.lock().contains(&hash) {
                continue;
            }
            match self.storage.has_node(&hash).await {
                Ok(true) => {
                    self.present_nodes.lock().insert(hash);
                }
                _ => {
                    if let Some(upstream) = self.upstream()
                        && upstream.has_node(hash).await.unwrap_or(false)
                    {
                        self.present_nodes.lock().insert(hash);
                        continue;
                    }
                    missing.push(hash);
                }
            }
        }
        missing
    }

    // =========================================================================
    // Cache key operations
    // =========================================================================

    async fn lookup(&self, cache_key: CacheKey) -> Option<NodeHash> {
        self.storage
            .get_cache(&cache_key)
            .await
            .ok()
            .flatten()
            .map(NodeHash)
    }

    async fn publish(&self, cache_key: CacheKey, manifest_hash: NodeHash) -> PublishResult {
        // Validate node exists
        if !self.storage.has_node(&manifest_hash).await.unwrap_or(false) {
            return PublishResult {
                success: false,
                error: Some(format!(
                    "manifest node {} does not exist",
                    manifest_hash.to_hex()
                )),
            };
        }

        // Write the cache mapping
        match self.storage.put_cache(&cache_key, &manifest_hash.0).await {
            Ok(()) => PublishResult {
                success: true,
                error: None,
            },
            Err(e) => PublishResult {
                success: false,
                error: Some(format!("failed to write cache mapping: {}", e)),
            },
        }
    }

    async fn unpublish(&self, cache_key: CacheKey) -> bool {
        match self.storage.delete_cache(&cache_key).await {
            Ok(deleted) => deleted,
            Err(e) => {
                tracing::warn!(cache_key = %cache_key, error = %e, "failed to delete cache entry");
                false
            }
        }
    }

    async fn lookup_toolchain_spec(&self, spec_key: ToolchainSpecKey) -> Option<NodeHash> {
        self.storage
            .get_toolchain_spec(&spec_key)
            .await
            .ok()
            .flatten()
            .map(NodeHash)
    }

    async fn get_toolchain_manifest(&self, manifest_hash: NodeHash) -> Option<ToolchainManifest> {
        let data = self.storage.get_node(&manifest_hash).await.ok().flatten()?;
        let node = CasNode::from_bytes(&data).ok()?;
        match node {
            CasNode::Toolchain(manifest) => Some(manifest),
            _ => {
                tracing::warn!(
                    manifest_hash = %manifest_hash,
                    "get_toolchain_manifest: node is not a Toolchain variant"
                );
                None
            }
        }
    }

    async fn get_materialization_plan(
        &self,
        manifest_hash: NodeHash,
    ) -> Option<MaterializationPlan> {
        let manifest = StoreService::get_toolchain_manifest(self, manifest_hash).await?;

        // Pure function: manifest → plan (no heuristics, bit-for-bit stable)
        let steps = match manifest.kind {
            ToolchainKind::Rust => {
                let mut steps = vec![MaterializeStep::EnsureDir {
                    relpath: "sysroot".to_string(),
                }];
                steps.extend(manifest.components.iter().map(|c| {
                    MaterializeStep::MaterializeTree {
                        tree_root: c.tree_root,
                        dest_subdir: "sysroot".to_string(),
                    }
                }));
                steps
            }
            ToolchainKind::Zig => {
                let mut steps = Vec::new();
                for c in &manifest.components {
                    match c.name.as_str() {
                        "zig-exe" | "zig-lib" => {
                            steps.push(MaterializeStep::MaterializeTree {
                                tree_root: c.tree_root,
                                dest_subdir: ".".to_string(),
                            });
                        }
                        _ => {} // Unknown component, skip
                    }
                }
                steps
            }
        };

        Some(MaterializationPlan {
            toolchain_id: manifest.toolchain_id,
            layout_version: MATERIALIZATION_LAYOUT_VERSION,
            steps,
        })
    }

    // =========================================================================
    // Registry operations
    // =========================================================================

    #[tracing::instrument(skip(self, progress), fields(name = %spec.name, version = %spec.version))]
    async fn ensure_registry_crate(
        &self,
        spec: RegistrySpec,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureRegistryCrateResult {
        // Validate spec and compute spec_key
        let spec_key = match spec.spec_key() {
            Ok(k) => k,
            Err(e) => {
                return EnsureRegistryCrateResult {
                    spec_key: None,
                    manifest_hash: None,
                    status: EnsureStatus::Failed,
                    error: Some(format!("invalid spec: {}", e)),
                };
            }
        };

        // Validate registry is crates.io
        if spec.registry_url != CRATES_IO_REGISTRY {
            return EnsureRegistryCrateResult {
                spec_key: Some(spec_key),
                manifest_hash: None,
                status: EnsureStatus::Failed,
                error: Some(format!(
                    "only crates.io is supported, got: {}",
                    spec.registry_url
                )),
            };
        }

        let this = self.clone();
        let this2 = self.clone();

        self.registry_manager
            .ensure(
                spec_key,
                async move || {
                    this.storage
                        .get_registry_spec(&spec_key)
                        .await
                        .ok()
                        .flatten()
                },
                async move || {
                    send_progress_best_effort(
                        &progress,
                        StoreProgress::message(
                            format!("downloading {}@{}", spec.name, spec.version),
                            None,
                            None,
                        ),
                    )
                    .await;

                    // Download tarball
                    let tarball_bytes =
                        match download_crate(&spec.name, &spec.version, &spec.checksum).await {
                            Ok(bytes) => bytes,
                            Err(e) => {
                                return EnsureRegistryCrateResult {
                                    spec_key: Some(spec_key),
                                    manifest_hash: None,
                                    status: EnsureStatus::Failed,
                                    error: Some(e.to_string()),
                                };
                            }
                        };

                    send_progress_best_effort(
                        &progress,
                        StoreProgress::message(
                            format!("storing {}@{}", spec.name, spec.version),
                            None,
                            None,
                        ),
                    )
                    .await;

                    // Store tarball as chunks
                    let prepared = prepare_blob(&tarball_bytes);
                    let tarball_ref =
                        match store_prepared_blob(&this2, &prepared, &tarball_bytes).await {
                            Ok(r) => r,
                            Err(e) => {
                                return EnsureRegistryCrateResult {
                                    spec_key: Some(spec_key),
                                    manifest_hash: None,
                                    status: EnsureStatus::Failed,
                                    error: Some(format!("failed to store tarball: {e}")),
                                };
                            }
                        };

                    // Create manifest as CasNode::RegistryCrate
                    let manifest = RegistryCrateManifest {
                        schema_version: REGISTRY_MANIFEST_SCHEMA_VERSION,
                        spec: spec.clone(),
                        crate_tarball: tarball_ref.clone(),
                    };

                    let node = CasNode::RegistryCrate(manifest);
                    let (node_bytes, manifest_hash) = node.to_bytes_and_hash();
                    if let Err(e) = this2.storage.put_node(&manifest_hash, &node_bytes).await {
                        tracing::error!(
                            manifest_hash = %manifest_hash,
                            error = %e,
                            "failed to write registry manifest node"
                        );
                        return EnsureRegistryCrateResult {
                            spec_key: Some(spec_key),
                            manifest_hash: None,
                            status: EnsureStatus::Failed,
                            error: Some(format!("failed to write registry manifest node: {e}")),
                        };
                    }

                    if let Err(e) = validate_blob_ref_closure(&this2, &tarball_ref).await {
                        return EnsureRegistryCrateResult {
                            spec_key: Some(spec_key),
                            manifest_hash: None,
                            status: EnsureStatus::Failed,
                            error: Some(format!("registry closure validation failed: {e}")),
                        };
                    }

                    // Publish spec → manifest_hash mapping and return canonical mapping winner.
                    match this2
                        .storage
                        .put_registry_spec(&spec_key, &manifest_hash.0)
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            let winner_hash = match this2.storage.get_registry_spec(&spec_key).await
                            {
                                Ok(Some(hash)) => hash,
                                Ok(None) => {
                                    return EnsureRegistryCrateResult {
                                        spec_key: Some(spec_key),
                                        manifest_hash: None,
                                        status: EnsureStatus::Failed,
                                        error: Some(format!(
                                            "registry spec mapping for {spec_key} already exists but could not be read"
                                        )),
                                    };
                                }
                                Err(e) => {
                                    return EnsureRegistryCrateResult {
                                        spec_key: Some(spec_key),
                                        manifest_hash: None,
                                        status: EnsureStatus::Failed,
                                        error: Some(format!(
                                            "failed to read winner registry mapping for {spec_key}: {e}"
                                        )),
                                    };
                                }
                            };
                            return EnsureRegistryCrateResult {
                                spec_key: Some(spec_key),
                                manifest_hash: Some(NodeHash(winner_hash)),
                                status: EnsureStatus::Hit,
                                error: None,
                            };
                        }
                        Err(e) => {
                            tracing::error!(
                                "failed to publish registry spec mapping for {}: {}",
                                spec_key,
                                e
                            );
                            return EnsureRegistryCrateResult {
                                spec_key: Some(spec_key),
                                manifest_hash: None,
                                status: EnsureStatus::Failed,
                                error: Some(format!("failed to publish registry spec mapping: {e}")),
                            };
                        }
                    }

                    tracing::info!(
                        name = %spec.name,
                        version = %spec.version,
                        spec_key = %spec_key,
                        manifest_hash = %manifest_hash,
                        "stored registry crate in CAS"
                    );

                    EnsureRegistryCrateResult {
                        spec_key: Some(spec_key),
                        manifest_hash: Some(manifest_hash),
                        status: EnsureStatus::Downloaded,
                        error: None,
                    }
                },
            )
            .await
    }

    async fn get_registry_manifest(
        &self,
        manifest_hash: NodeHash,
    ) -> Option<RegistryCrateManifest> {
        let data = self.storage.get_node(&manifest_hash).await.ok().flatten()?;
        let node = CasNode::from_bytes(&data).ok()?;
        match node {
            CasNode::RegistryCrate(manifest) => Some(manifest),
            _ => {
                tracing::warn!(
                    manifest_hash = %manifest_hash,
                    "get_registry_manifest: node is not a RegistryCrate variant"
                );
                None
            }
        }
    }

    async fn lookup_registry_spec(&self, spec_key: RegistrySpecKey) -> Option<NodeHash> {
        self.storage
            .get_registry_spec(&spec_key)
            .await
            .ok()
            .flatten()
            .map(NodeHash)
    }
}

impl StoreRpc for StoreService {
    async fn version(&self) -> ServiceVersion {
        StoreService::version(self).await
    }

    async fn put_chunk(&self, hash: ChunkHash, data: Vec<u8>) -> Result<(), String> {
        StoreService::put_chunk(self, hash, data).await
    }

    async fn get_chunk(&self, hash: ChunkHash) -> Option<Vec<u8>> {
        StoreService::get_chunk(self, hash).await
    }

    async fn has_chunk(&self, hash: ChunkHash) -> bool {
        StoreService::has_chunk(self, hash).await
    }

    async fn list_missing_chunks(&self, hashes: Vec<ChunkHash>) -> Vec<ChunkHash> {
        StoreService::list_missing_chunks(self, hashes).await
    }

    async fn put_node(&self, hash: NodeHash, data: Vec<u8>) -> Result<(), String> {
        StoreService::put_node(self, hash, data).await
    }

    async fn get_node(&self, hash: NodeHash) -> Option<Vec<u8>> {
        StoreService::get_node(self, hash).await
    }

    async fn has_node(&self, hash: NodeHash) -> bool {
        StoreService::has_node(self, hash).await
    }

    async fn list_missing_nodes(&self, hashes: Vec<NodeHash>) -> Vec<NodeHash> {
        StoreService::list_missing_nodes(self, hashes).await
    }

    async fn lookup(&self, cache_key: CacheKey) -> Option<NodeHash> {
        StoreService::lookup(self, cache_key).await
    }

    async fn publish(&self, cache_key: CacheKey, manifest_hash: NodeHash) -> PublishResult {
        StoreService::publish(self, cache_key, manifest_hash).await
    }

    async fn unpublish(&self, cache_key: CacheKey) -> bool {
        StoreService::unpublish(self, cache_key).await
    }

    async fn ensure_rust_toolchain(
        &self,
        spec: RustToolchainSpec,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureToolchainResult {
        StoreService::ensure_rust_toolchain(self, spec, progress).await
    }

    async fn download_rust_toolchain_component(
        &self,
        spec: RustToolchainSpec,
        component: RustComponent,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureRustToolchainComponentResult {
        StoreService::download_rust_toolchain_component(self, spec, component, progress).await
    }

    async fn extract_rust_toolchain_component(
        &self,
        spec: RustToolchainSpec,
        component: RustComponent,
        tarball: BlobRef,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureRustToolchainComponentResult {
        StoreService::extract_rust_toolchain_component(self, spec, component, tarball, progress)
            .await
    }

    async fn finalize_rust_toolchain(&self, spec: RustToolchainSpec) -> EnsureToolchainResult {
        StoreService::finalize_rust_toolchain(self, spec).await
    }

    async fn ensure_zig_toolchain(
        &self,
        spec: ZigToolchainSpec,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureToolchainResult {
        StoreService::ensure_zig_toolchain(self, spec, progress).await
    }

    async fn get_toolchain_manifest(&self, manifest_hash: NodeHash) -> Option<ToolchainManifest> {
        StoreService::get_toolchain_manifest(self, manifest_hash).await
    }

    async fn get_materialization_plan(
        &self,
        manifest_hash: NodeHash,
    ) -> Option<MaterializationPlan> {
        StoreService::get_materialization_plan(self, manifest_hash).await
    }

    async fn lookup_toolchain_spec(&self, spec_key: ToolchainSpecKey) -> Option<NodeHash> {
        StoreService::lookup_toolchain_spec(self, spec_key).await
    }

    async fn ensure_registry_crate(
        &self,
        spec: RegistrySpec,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureRegistryCrateResult {
        StoreService::ensure_registry_crate(self, spec, progress).await
    }

    async fn get_registry_manifest(
        &self,
        manifest_hash: NodeHash,
    ) -> Option<RegistryCrateManifest> {
        StoreService::get_registry_manifest(self, manifest_hash).await
    }

    async fn lookup_registry_spec(&self, spec_key: RegistrySpecKey) -> Option<NodeHash> {
        StoreService::lookup_registry_spec(self, spec_key).await
    }
}

// =============================================================================
// Validation Helpers
// =============================================================================

fn validate_blob_node(blob: &vx_store_proto::BlobNode) -> Result<(), String> {
    // chunks.len() >= 2 (single-chunk files should be SmallFile)
    if blob.chunks.len() < 2 {
        return Err(format!(
            "BlobNode must have at least 2 chunks, got {}",
            blob.chunks.len()
        ));
    }

    // chunks.len() <= MAX_CHUNKS_PER_BLOB
    if blob.chunks.len() > MAX_CHUNKS_PER_BLOB {
        return Err(format!(
            "BlobNode has {} chunks (max {})",
            blob.chunks.len(),
            MAX_CHUNKS_PER_BLOB
        ));
    }

    // Validate each chunk size
    let mut sum: u64 = 0;
    for (i, chunk) in blob.chunks.iter().enumerate() {
        if chunk.size == 0 || chunk.size > CDC_MAX_CHUNK {
            return Err(format!(
                "chunk[{i}] size {} is out of range 1..={}",
                chunk.size, CDC_MAX_CHUNK
            ));
        }
        sum += chunk.size as u64;
    }

    // sum(chunk.size) == total_size
    if sum != blob.total_size {
        return Err(format!(
            "BlobNode total_size mismatch: sum of chunks is {sum}, declared {}",
            blob.total_size
        ));
    }

    // total_size > CDC_MAX_CHUNK (otherwise it should be a SmallFile)
    if blob.total_size <= CDC_MAX_CHUNK as u64 {
        return Err(format!(
            "BlobNode total_size {} is <= CDC_MAX_CHUNK ({}), should be SmallFile",
            blob.total_size, CDC_MAX_CHUNK
        ));
    }

    // cdc_params_id must be known
    if !KNOWN_CDC_PARAMS
        .iter()
        .any(|(id, _, _, _)| *id == blob.cdc_params_id)
    {
        return Err(format!("unknown cdc_params_id: {}", blob.cdc_params_id));
    }

    Ok(())
}

fn validate_directory_node(dir: &DirectoryNode) -> Result<(), String> {
    let entries = &dir.entries;

    for (i, entry) in entries.iter().enumerate() {
        // No empty names
        if entry.name.is_empty() {
            return Err(format!("entry[{i}] has empty name"));
        }

        // No "." or ".."
        if entry.name == "." || entry.name == ".." {
            return Err(format!("entry[{i}] has forbidden name \"{}\"", entry.name));
        }

        // No "/" in names
        if entry.name.contains('/') {
            return Err(format!("entry[{i}] name \"{}\" contains '/'", entry.name));
        }

        // Validate SmallFile/LargeFile size constraints
        match &entry.kind {
            DirEntryKind::SmallFile { size, .. } => {
                if *size > CDC_MAX_CHUNK as u64 {
                    return Err(format!(
                        "entry[{i}] \"{}\" is SmallFile but size {} > CDC_MAX_CHUNK ({})",
                        entry.name, size, CDC_MAX_CHUNK
                    ));
                }
            }
            DirEntryKind::LargeFile { size, .. } => {
                if *size <= CDC_MAX_CHUNK as u64 {
                    return Err(format!(
                        "entry[{i}] \"{}\" is LargeFile but size {} <= CDC_MAX_CHUNK ({})",
                        entry.name, size, CDC_MAX_CHUNK
                    ));
                }
            }
            DirEntryKind::Dir { .. } | DirEntryKind::Symlink { .. } => {}
        }

        // Check sorted order and no duplicates
        if i > 0 {
            let prev = &entries[i - 1].name;
            match prev.as_bytes().cmp(entry.name.as_bytes()) {
                std::cmp::Ordering::Greater => {
                    return Err(format!(
                        "entries not sorted: \"{}\" comes after \"{}\"",
                        entry.name, prev
                    ));
                }
                std::cmp::Ordering::Equal => {
                    return Err(format!("duplicate entry name: \"{}\"", entry.name));
                }
                std::cmp::Ordering::Less => {}
            }
        }
    }

    Ok(())
}

// =============================================================================
// Internal Helpers
// =============================================================================

/// Store a prepared blob (chunks + optional BlobNode) into the store.
/// Returns a BlobRef describing how to fetch the data.
async fn store_prepared_blob(
    service: &StoreService,
    prepared: &PreparedBlob,
    data: &[u8],
) -> Result<BlobRef, String> {
    // Store all chunks
    for chunk_info in &prepared.chunks {
        let chunk_data = &data[chunk_info.offset..chunk_info.offset + chunk_info.size as usize];
        service
            .storage
            .put_chunk(&chunk_info.hash, chunk_data)
            .await
            .map_err(|e| format!("failed to store chunk: {e}"))?;
    }

    // If multi-chunk, store the BlobNode
    if let Some(blob_node) = &prepared.blob_node {
        service
            .storage
            .put_node(&blob_node.node_hash, &blob_node.serialized_bytes)
            .await
            .map_err(|e| format!("failed to store BlobNode: {e}"))?;

        Ok(BlobRef::Large {
            blob_node: blob_node.node_hash,
            content_hash: prepared.content_hash,
            size: data.len() as u64,
        })
    } else {
        // Single chunk
        assert_eq!(prepared.chunks.len(), 1);
        Ok(BlobRef::Small {
            chunk: prepared.chunks[0].hash,
            size: data.len() as u64,
        })
    }
}

async fn validate_blob_ref_closure(
    service: &StoreService,
    blob_ref: &BlobRef,
) -> Result<(), String> {
    match blob_ref {
        BlobRef::Small { chunk, .. } => {
            let exists = service
                .storage
                .has_chunk(chunk)
                .await
                .map_err(|e| format!("failed to check chunk {}: {e}", chunk.to_hex()))?;
            if !exists {
                return Err(format!("missing chunk {}", chunk.to_hex()));
            }
        }
        BlobRef::Large { blob_node, .. } => {
            let blob_data = service
                .storage
                .get_node(blob_node)
                .await
                .map_err(|e| format!("failed to read BlobNode {}: {e}", blob_node.to_hex()))?
                .ok_or_else(|| format!("missing BlobNode {}", blob_node.to_hex()))?;
            let node = CasNode::from_bytes(&blob_data).map_err(|e| {
                format!("failed to deserialize BlobNode {}: {e}", blob_node.to_hex())
            })?;
            let blob = match node {
                CasNode::Blob(blob) => blob,
                _ => return Err(format!("node {} is not a BlobNode", blob_node.to_hex())),
            };
            for chunk in blob.chunks {
                let exists =
                    service.storage.has_chunk(&chunk.hash).await.map_err(|e| {
                        format!("failed to check chunk {}: {e}", chunk.hash.to_hex())
                    })?;
                if !exists {
                    return Err(format!(
                        "BlobNode {} references missing chunk {}",
                        blob_node.to_hex(),
                        chunk.hash.to_hex()
                    ));
                }
            }
        }
    }

    Ok(())
}
