use jiff::Timestamp;
use vx_store_proto::Store;
use vx_store_proto::{
    Blake3Hash, BlobHash, CacheKey, EnsureRegistryCrateResult, EnsureStatus, EnsureToolchainResult,
    IngestTreeRequest, IngestTreeResult, ManifestHash, MaterializationPlan, MaterializeStep,
    NodeManifest, PublishResult, RegistryCrateManifest, RegistrySpec, RegistrySpecKey,
    RustToolchainSpec, STORE_PROTOCOL_VERSION, ServiceVersion, StoreProgress,
    TREE_MANIFEST_SCHEMA_VERSION, ToolchainKind, ToolchainManifest, ToolchainSpecKey, TreeManifest,
    ZigToolchainSpec,
};

use crate::registry::download_crate;
use crate::types::StoreService;

const CRATES_IO_REGISTRY: &str = "https://crates.io";
const REGISTRY_MANIFEST_SCHEMA_VERSION: u32 = 1;
const MATERIALIZATION_LAYOUT_VERSION: u32 = 1;

impl Store for StoreService {
    async fn version(&self, _cx: &roam::Context) -> ServiceVersion {
        ServiceVersion {
            service: "vx-store".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: STORE_PROTOCOL_VERSION,
        }
    }

    async fn lookup(&self, _cx: &roam::Context, cache_key: CacheKey) -> Option<ManifestHash> {
        self.storage.get_cache(&cache_key).await.ok().flatten()
    }

    async fn publish(
        &self,
        _cx: &roam::Context,
        cache_key: CacheKey,
        manifest_hash: ManifestHash,
    ) -> PublishResult {
        // Validate manifest blob exists
        if !self.storage.has_blob(&manifest_hash).await.unwrap_or(false) {
            return PublishResult {
                success: false,
                error: Some(format!(
                    "manifest {} does not exist",
                    manifest_hash.to_hex()
                )),
            };
        }

        // Write the cache mapping
        match self.storage.put_cache(&cache_key, &manifest_hash).await {
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

    async fn unpublish(&self, _cx: &roam::Context, cache_key: CacheKey) -> bool {
        match self.storage.delete_cache(&cache_key).await {
            Ok(deleted) => deleted,
            Err(e) => {
                tracing::warn!(cache_key = %cache_key, error = %e, "failed to delete cache entry");
                false
            }
        }
    }

    // [impl axiom.cas-unified-blobs] Node manifests are stored as blobs
    async fn put_manifest(&self, _cx: &roam::Context, manifest: NodeManifest) -> ManifestHash {
        let json =
            facet_json::to_string(&manifest).expect("node manifest serialization must succeed");
        let hash = ManifestHash::from_bytes(json.as_bytes());

        if let Err(e) = self.storage.put_blob(&hash, json.as_bytes()).await {
            tracing::warn!(manifest_hash = %hash, error = %e, "failed to write node manifest blob");
        }

        hash
    }

    // [impl axiom.cas-unified-blobs] Node manifests are read from blob store
    async fn get_manifest(&self, _cx: &roam::Context, hash: ManifestHash) -> Option<NodeManifest> {
        tracing::debug!(manifest_hash = %hash, "get_manifest: reading from blob store");

        let data = self.storage.get_blob(&hash).await.ok().flatten()?;
        let json = String::from_utf8(data).ok()?;

        tracing::debug!(manifest_hash = %hash, "get_manifest: blob read successfully");
        match facet_json::from_str(&json) {
            Ok(manifest) => {
                tracing::debug!(manifest_hash = %hash, "get_manifest: parsed successfully");
                Some(manifest)
            }
            Err(e) => {
                tracing::warn!(manifest_hash = %hash, error = ?e, "get_manifest: failed to parse JSON");
                None
            }
        }
    }

    async fn put_blob(&self, _cx: &roam::Context, data: Vec<u8>) -> BlobHash {
        let hash = BlobHash::from_bytes(&data);

        if let Err(e) = self.storage.put_blob(&hash, &data).await {
            tracing::warn!(blob_hash = %hash, error = %e, "failed to write blob");
        }

        hash
    }

    async fn get_blob(&self, _cx: &roam::Context, hash: BlobHash) -> Option<Vec<u8>> {
        self.storage.get_blob(&hash).await.ok().flatten()
    }

    async fn get_blob_range(
        &self,
        _cx: &roam::Context,
        hash: BlobHash,
        offset: u64,
        len: u64,
    ) -> Option<Vec<u8>> {
        self.storage
            .get_blob_range(&hash, offset, len)
            .await
            .ok()
            .flatten()
    }

    async fn has_blob(&self, _cx: &roam::Context, hash: BlobHash) -> bool {
        self.storage.has_blob(&hash).await.unwrap_or(false)
    }

    #[tracing::instrument(skip(self, progress), fields(name = %spec.name, version = %spec.version))]
    async fn ensure_registry_crate(
        &self,
        _cx: &roam::Context,
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
                async move || this.storage.get_registry_spec(&spec_key).await.ok().flatten(),
                async move || {
                    let _ = progress
                        .send(&StoreProgress {
                            message: format!("downloading {}@{}", spec.name, spec.version),
                            current: None,
                            total: None,
                        })
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

                    let _ = progress
                        .send(&StoreProgress {
                            message: format!("storing {}@{}", spec.name, spec.version),
                            current: None,
                            total: None,
                        })
                        .await;

                    // Store tarball as blob
                    let tarball_blob = BlobHash::from_bytes(&tarball_bytes);
                    if let Err(e) = this2
                        .storage
                        .put_blob(&tarball_blob, &tarball_bytes)
                        .await
                    {
                        tracing::warn!(blob_hash = %tarball_blob, error = %e, "failed to write tarball blob");
                    }

                    // Create manifest
                    let manifest = RegistryCrateManifest {
                        schema_version: REGISTRY_MANIFEST_SCHEMA_VERSION,
                        spec: spec.clone(),
                        crate_tarball_blob: tarball_blob,
                        created_at: Timestamp::now().in_tz("UTC").unwrap().to_string(),
                    };

                    // Store manifest
                    let manifest_json = facet_json::to_string(&manifest)
                        .expect("registry manifest serialization must succeed");
                    let manifest_hash = Blake3Hash::from_bytes(manifest_json.as_bytes());
                    if let Err(e) = this2
                        .storage
                        .put_manifest(&manifest_hash, manifest_json.as_bytes())
                        .await
                    {
                        tracing::warn!(manifest_hash = %manifest_hash, error = %e, "failed to write registry manifest");
                    }

                    // Publish spec → manifest_hash mapping
                    if let Err(e) = this2
                        .storage
                        .put_registry_spec(&spec_key, &manifest_hash)
                        .await
                    {
                        tracing::warn!(
                            "failed to publish registry spec mapping for {}: {}",
                            spec_key,
                            e
                        );
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
        _cx: &roam::Context,
        manifest_hash: Blake3Hash,
    ) -> Option<RegistryCrateManifest> {
        let json = match self.storage.get_manifest(&manifest_hash).await {
            Ok(Some(json)) => json,
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(manifest_hash = %manifest_hash, error = %e, "get_registry_manifest: read error");
                return None;
            }
        };
        match facet_json::from_str(&json) {
            Ok(manifest) => Some(manifest),
            Err(e) => {
                tracing::warn!(manifest_hash = %manifest_hash, error = ?e, "get_registry_manifest: failed to parse JSON");
                None
            }
        }
    }

    async fn lookup_registry_spec(
        &self,
        _cx: &roam::Context,
        spec_key: RegistrySpecKey,
    ) -> Option<Blake3Hash> {
        self.storage
            .get_registry_spec(&spec_key)
            .await
            .ok()
            .flatten()
    }

    async fn ensure_rust_toolchain(
        &self,
        _cx: &roam::Context,
        spec: RustToolchainSpec,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureToolchainResult {
        StoreService::ensure_rust_toolchain(self, spec, progress).await
    }

    async fn ensure_zig_toolchain(
        &self,
        _cx: &roam::Context,
        spec: ZigToolchainSpec,
        progress: roam::Tx<StoreProgress>,
    ) -> EnsureToolchainResult {
        StoreService::ensure_zig_toolchain(self, spec, progress).await
    }

    async fn lookup_toolchain_spec(
        &self,
        _cx: &roam::Context,
        spec_key: ToolchainSpecKey,
    ) -> Option<Blake3Hash> {
        // Delegate to the inherent method
        self.storage
            .get_toolchain_spec(&spec_key)
            .await
            .ok()
            .flatten()
    }

    async fn get_toolchain_manifest(
        &self,
        _cx: &roam::Context,
        manifest_hash: Blake3Hash,
    ) -> Option<ToolchainManifest> {
        tracing::debug!(manifest_hash = %manifest_hash, "get_toolchain_manifest: attempting to read");

        let json = match self.storage.get_manifest(&manifest_hash).await {
            Ok(Some(json)) => json,
            Ok(None) => {
                tracing::warn!(manifest_hash = %manifest_hash, "get_toolchain_manifest: not found");
                return None;
            }
            Err(e) => {
                tracing::warn!(manifest_hash = %manifest_hash, error = %e, "get_toolchain_manifest: read error");
                return None;
            }
        };

        tracing::debug!(manifest_hash = %manifest_hash, "get_toolchain_manifest: read successfully");
        match facet_json::from_str(&json) {
            Ok(manifest) => {
                tracing::debug!(manifest_hash = %manifest_hash, "get_toolchain_manifest: parsed successfully");
                Some(manifest)
            }
            Err(e) => {
                tracing::warn!(manifest_hash = %manifest_hash, error = ?e, "get_toolchain_manifest: failed to parse JSON");
                None
            }
        }
    }

    async fn get_materialization_plan(
        &self,
        _cx: &roam::Context,
        manifest_hash: Blake3Hash,
    ) -> Option<MaterializationPlan> {
        let manifest = Store::get_toolchain_manifest(self, _cx, manifest_hash).await?;

        // Pure function: manifest → plan (no heuristics, bit-for-bit stable)
        let steps = match manifest.kind {
            ToolchainKind::Rust => {
                let mut steps = vec![MaterializeStep::EnsureDir {
                    relpath: "sysroot".to_string(),
                }];
                // Materialize each component tree into sysroot
                steps.extend(manifest.components.iter().map(|c| {
                    MaterializeStep::MaterializeTree {
                        tree_manifest: c.tree_manifest,
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
                                tree_manifest: c.tree_manifest,
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

    async fn ingest_tree(
        &self,
        _cx: &roam::Context,
        request: IngestTreeRequest,
    ) -> IngestTreeResult {
        let mut manifest = TreeManifest {
            schema_version: TREE_MANIFEST_SCHEMA_VERSION,
            entries: Vec::with_capacity(request.files.len()),
            total_size_bytes: 0,
            unique_blobs: 0,
        };

        let mut new_blobs = 0u32;
        let mut total_bytes = 0u64;

        for file in &request.files {
            let size = file.contents.len() as u64;
            total_bytes += size;

            // Store the blob
            let blob_hash = BlobHash::from_bytes(&file.contents);

            // Check if blob already exists (for dedup stats)
            let exists = self.storage.has_blob(&blob_hash).await.unwrap_or(false);
            if !exists {
                if let Err(e) = self.storage.put_blob(&blob_hash, &file.contents).await {
                    return IngestTreeResult {
                        success: false,
                        manifest_hash: None,
                        file_count: 0,
                        total_bytes: 0,
                        new_blobs: 0,
                        error: Some(format!("failed to write blob for {}: {}", file.path, e)),
                    };
                }
                new_blobs += 1;
            }

            // Add to manifest
            manifest.add_file(file.path.clone(), blob_hash, size, file.executable);
        }

        // Finalize manifest (sorts entries, computes unique blob count)
        manifest.finalize();

        // Serialize manifest to JSON
        let manifest_json =
            facet_json::to_string(&manifest).expect("tree manifest serialization must succeed");
        let manifest_hash = ManifestHash::from_bytes(manifest_json.as_bytes());

        // Store as blob (for runner's materialize_tree_from_cas which calls get_blob)
        if let Err(e) = self
            .storage
            .put_blob(&manifest_hash, manifest_json.as_bytes())
            .await
        {
            return IngestTreeResult {
                success: false,
                manifest_hash: None,
                file_count: 0,
                total_bytes: 0,
                new_blobs: 0,
                error: Some(format!("failed to write tree manifest blob: {}", e)),
            };
        }

        // Also store in tree_manifests (for get_tree_manifest API)
        if let Err(e) = self
            .storage
            .put_tree_manifest(&manifest_hash, manifest_json.as_bytes())
            .await
        {
            return IngestTreeResult {
                success: false,
                manifest_hash: None,
                file_count: 0,
                total_bytes: 0,
                new_blobs: 0,
                error: Some(format!("failed to write tree manifest: {}", e)),
            };
        }

        tracing::info!(
            manifest_hash = %manifest_hash,
            file_count = request.files.len(),
            total_bytes,
            new_blobs,
            unique_blobs = manifest.unique_blobs,
            "ingested tree"
        );

        IngestTreeResult {
            success: true,
            manifest_hash: Some(manifest_hash),
            file_count: request.files.len() as u32,
            total_bytes,
            new_blobs,
            error: None,
        }
    }

    async fn get_tree_manifest(
        &self,
        _cx: &roam::Context,
        hash: ManifestHash,
    ) -> Option<TreeManifest> {
        tracing::debug!(manifest_hash = %hash, "get_tree_manifest: reading from blob storage");

        let data = self.storage.get_blob(&hash).await.ok().flatten()?;
        let json = match String::from_utf8(data) {
            Ok(json) => json,
            Err(e) => {
                tracing::error!(manifest_hash = %hash, error = %e, "get_tree_manifest: invalid UTF-8");
                return None;
            }
        };

        tracing::debug!(manifest_hash = %hash, "get_tree_manifest: blob read successfully");
        match facet_json::from_str(&json) {
            Ok(manifest) => {
                tracing::debug!(manifest_hash = %hash, "get_tree_manifest: parsed successfully");
                Some(manifest)
            }
            Err(e) => {
                tracing::error!(manifest_hash = %hash, error = ?e, "get_tree_manifest: failed to parse JSON");
                None
            }
        }
    }

    async fn stream_blob(
        &self,
        _cx: &roam::Context,
        blob: Blake3Hash,
        output: roam::Tx<Vec<u8>>,
    ) -> Result<(), String> {
        let blob_path = self
            .storage
            .blob_path(&blob)
            .await
            .map_err(|e| format!("blob lookup error: {e}"))?
            .ok_or_else(|| "blob not found".to_string())?;

        use tokio::io::AsyncReadExt;

        let file = tokio::fs::File::open(&blob_path)
            .await
            .map_err(|e| format!("failed to open blob: {e}"))?;

        let mut reader = tokio::io::BufReader::new(file);
        let mut buf = vec![0u8; 16 * 1024 * 1024]; // 16MB chunks

        loop {
            match reader.read(&mut buf).await {
                Ok(0) => break, // EOF
                Ok(n) => {
                    if output.send(&buf[..n].to_vec()).await.is_err() {
                        break; // Receiver dropped
                    }
                }
                Err(e) => return Err(format!("read error: {e}")),
            }
        }

        Ok(())
    }
}
