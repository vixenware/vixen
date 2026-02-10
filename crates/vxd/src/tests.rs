use super::*;
use tokio_test_lite::test as vx_test;
use vx_store_proto::Blake3Hash;

/// Create a fresh database for testing
fn test_db() -> Database {
    Database::new()
}

/// Set up common build configuration for C builds
fn setup_cc_config(db: &Database) -> BuildConfig {
    let build_key = BuildConfig::compute_key("debug", "x86_64-linux-musl", "/test/workspace", "");
    let config = BuildConfig::new(
        db,
        build_key,
        "debug".to_string(),
        "x86_64-linux-musl".to_string(),
        "/test/workspace".to_string(),
        String::new(),
    )
    .unwrap();

    ZigToolchainConfig::set(db, "0.13.0".to_string(), "zig:abc123def456".to_string()).unwrap();

    config
}

#[vx_test]
async fn test_cache_key_cc_compile_deterministic() {
    let db = test_db();
    let config = setup_cc_config(&db);

    let source_hash = Blake3Hash::from_bytes(b"int main() { return 0; }");

    // Create source file input
    let source = CSourceFile::new(&db, "src/main.c".to_string(), source_hash).unwrap();

    // Compute cache key twice
    let key1 = cache_key_cc_compile(&db, source, config).await.unwrap();
    let key2 = cache_key_cc_compile(&db, source, config).await.unwrap();

    // Should be identical (deterministic)
    assert_eq!(key1, key2);
}

#[vx_test]
async fn test_cache_key_cc_compile_changes_with_source() {
    let db = test_db();
    let config = setup_cc_config(&db);

    // First source
    let source1 = CSourceFile::new(
        &db,
        "src/main.c".to_string(),
        Blake3Hash::from_bytes(b"int main() { return 0; }"),
    )
    .unwrap();

    let key1 = cache_key_cc_compile(&db, source1, config).await.unwrap();

    // Different source content
    let source2 = CSourceFile::new(
        &db,
        "src/main.c".to_string(),
        Blake3Hash::from_bytes(b"int main() { return 1; }"),
    )
    .unwrap();

    let key2 = cache_key_cc_compile(&db, source2, config).await.unwrap();

    // Keys should be different
    assert_ne!(key1, key2);
}

#[vx_test]
async fn test_cache_key_cc_compile_changes_with_profile() {
    let db = test_db();

    ZigToolchainConfig::set(&db, "0.13.0".to_string(), "zig:abc123def456".to_string()).unwrap();

    let source_hash = Blake3Hash::from_bytes(b"int main() { return 0; }");

    // Debug profile
    let build_key_debug =
        BuildConfig::compute_key("debug", "x86_64-linux-musl", "/test/workspace", "");
    let source_debug = CSourceFile::new(&db, "src/main.c".to_string(), source_hash).unwrap();
    let config_debug = BuildConfig::new(
        &db,
        build_key_debug,
        "debug".to_string(),
        "x86_64-linux-musl".to_string(),
        "/test/workspace".to_string(),
        String::new(),
    )
    .unwrap();
    let key_debug = cache_key_cc_compile(&db, source_debug, config_debug)
        .await
        .unwrap();

    // Release profile
    let build_key_release =
        BuildConfig::compute_key("release", "x86_64-linux-musl", "/test/workspace", "");
    let config_release = BuildConfig::new(
        &db,
        build_key_release,
        "release".to_string(),
        "x86_64-linux-musl".to_string(),
        "/test/workspace".to_string(),
        String::new(),
    )
    .unwrap();

    let source_release = CSourceFile::new(&db, "src/main.c".to_string(), source_hash).unwrap();
    let key_release = cache_key_cc_compile(&db, source_release, config_release)
        .await
        .unwrap();

    // Keys should be different
    assert_ne!(key_debug, key_release);
}

#[vx_test]
async fn test_node_id_cc_compile_format() {
    let db = test_db();
    let config = setup_cc_config(&db);

    let source = CSourceFile::new(
        &db,
        "src/main.c".to_string(),
        Blake3Hash::from_bytes(b"int main() { return 0; }"),
    )
    .unwrap();

    let node_id = node_id_cc_compile(&db, source, config).await.unwrap();

    // Should be formatted as "cc-compile:path:triple:profile"
    assert!(node_id.0.starts_with("cc-compile:"));
    assert!(node_id.0.contains("src/main.c"));
    assert!(node_id.0.contains("x86_64-linux-musl"));
    assert!(node_id.0.contains("debug"));
}

#[vx_test]
async fn test_cache_key_cc_link_deterministic() {
    let db = test_db();
    let config = setup_cc_config(&db);

    let obj_hashes = vec![
        Blake3Hash::from_bytes(b"main.o contents"),
        Blake3Hash::from_bytes(b"util.o contents"),
    ];

    let target = CTarget::new(
        &db,
        "hello".to_string(),
        obj_hashes.clone(),
        vec!["src/main.c".to_string(), "src/util.c".to_string()],
    )
    .unwrap();

    let key1 = cache_key_cc_link(&db, target, config).await.unwrap();
    let key2 = cache_key_cc_link(&db, target, config).await.unwrap();

    assert_eq!(key1, key2);
}

#[vx_test]
async fn test_cache_key_cc_link_changes_with_objects() {
    let db = test_db();
    let config = setup_cc_config(&db);

    // First set of objects
    let target1 = CTarget::new(
        &db,
        "hello".to_string(),
        vec![Blake3Hash::from_bytes(b"main.o v1")],
        vec!["src/main.c".to_string()],
    )
    .unwrap();

    let key1 = cache_key_cc_link(&db, target1, config).await.unwrap();

    // Different object contents
    let target2 = CTarget::new(
        &db,
        "hello".to_string(),
        vec![Blake3Hash::from_bytes(b"main.o v2")],
        vec!["src/main.c".to_string()],
    )
    .unwrap();

    let key2 = cache_key_cc_link(&db, target2, config).await.unwrap();

    assert_ne!(key1, key2);
}

#[vx_test]
async fn test_node_id_cc_link_format() {
    let db = test_db();
    let config = setup_cc_config(&db);

    let target = CTarget::new(
        &db,
        "hello".to_string(),
        vec![Blake3Hash::from_bytes(b"main.o")],
        vec!["src/main.c".to_string()],
    )
    .unwrap();

    let node_id = node_id_cc_link(&db, target, config).await.unwrap();

    // Should be formatted as "cc-link:name:triple:profile"
    assert!(node_id.0.starts_with("cc-link:"));
    assert!(node_id.0.contains("hello"));
    assert!(node_id.0.contains("x86_64-linux-musl"));
    assert!(node_id.0.contains("debug"));
}

// =============================================================================
// GOLDEN-FILE TESTS: Cache Key Stability
// =============================================================================
//
// These tests pin exact cache key values for known inputs. If any test fails,
// it means cache key computation changed — which silently invalidates every
// user's cache. That's either:
//   (a) intentional: update the golden value and note what changed
//   (b) accidental: revert whatever broke it
//
// See: https://github.com/bearcove/vixen/issues/131

/// Fixed toolchain ID used across all golden tests (blake3 of "golden-toolchain-id")
fn golden_toolchain_id() -> Blake3Hash {
    Blake3Hash::from_bytes(b"golden-toolchain-id")
}

/// Fixed toolchain manifest hash used across all golden tests
fn golden_toolchain_manifest() -> Blake3Hash {
    Blake3Hash::from_bytes(b"golden-toolchain-manifest")
}

/// Fixed source closure hash used across all golden tests
fn golden_closure_hash() -> Blake3Hash {
    Blake3Hash::from_bytes(b"golden-source-closure")
}

/// Fixed cargo.toml content hash used across all golden tests
fn golden_cargo_hash() -> Blake3Hash {
    Blake3Hash::from_bytes(b"golden-cargo-toml")
}

fn setup_golden_rust(db: &Database) -> (RustToolchain, BuildConfig) {
    let toolchain = RustToolchain::new(
        db,
        golden_toolchain_id(),
        golden_toolchain_manifest(),
        "x86_64-unknown-linux-gnu".to_string(),
        "x86_64-unknown-linux-gnu".to_string(),
    )
    .unwrap();

    let build_key = BuildConfig::compute_key(
        "release",
        "x86_64-unknown-linux-gnu",
        "/home/user/project",
        "",
    );
    let config = BuildConfig::new(
        db,
        build_key,
        "release".to_string(),
        "x86_64-unknown-linux-gnu".to_string(),
        "/home/user/project".to_string(),
        String::new(),
    )
    .unwrap();

    (toolchain, config)
}

#[vx_test]
async fn golden_cache_key_compile_bin() {
    let db = test_db();
    let (toolchain, config) = setup_golden_rust(&db);

    CargoToml::set(
        &db,
        golden_cargo_hash(),
        "hello-world".to_string(),
        vx_manifest::Edition::E2021,
        "src/main.rs".to_string(),
    )
    .unwrap();
    SourceClosure::set(&db, golden_closure_hash(), vec!["src/main.rs".to_string()]).unwrap();

    let key = cache_key_compile_bin(&db, toolchain, config).await.unwrap();
    assert_eq!(
        key.to_hex(),
        "5643b5f60dd42579ac9e556ac248a210cfd0ad3495e55c1e3b302c35feacb55e",
        "cache_key_compile_bin golden value changed! This invalidates all user caches. \
         If intentional, update this value."
    );
}

#[vx_test]
async fn golden_cache_key_compile_rlib() {
    let db = test_db();
    let (toolchain, config) = setup_golden_rust(&db);

    let crate_info = RustCrate::new(
        &db,
        "crate-abc123".to_string(),
        "my_lib".to_string(),
        "2021".to_string(),
        "lib".to_string(),
        "src/lib.rs".to_string(),
        golden_closure_hash(),
    )
    .unwrap();

    let key = cache_key_compile_rlib(&db, crate_info, toolchain, config)
        .await
        .unwrap();
    assert_eq!(
        key.to_hex(),
        "7bb67e2007282c850c38320be1be8a40a7158d2dda2c7d6f3de84d887359a8bb",
        "cache_key_compile_rlib golden value changed! This invalidates all user caches. \
         If intentional, update this value."
    );
}

#[vx_test]
async fn golden_cache_key_compile_rlib_with_deps() {
    let db = test_db();
    let (toolchain, config) = setup_golden_rust(&db);

    let crate_info = RustCrate::new(
        &db,
        "crate-abc123".to_string(),
        "my_lib".to_string(),
        "2021".to_string(),
        "lib".to_string(),
        "src/lib.rs".to_string(),
        golden_closure_hash(),
    )
    .unwrap();

    let dep_hashes = vec![
        ("log".to_string(), Blake3Hash::from_bytes(b"log-rlib-hash")),
        (
            "serde".to_string(),
            Blake3Hash::from_bytes(b"serde-rlib-hash"),
        ),
    ];

    let key = cache_key_compile_rlib_with_deps(&db, crate_info, toolchain, config, dep_hashes)
        .await
        .unwrap();
    assert_eq!(
        key.to_hex(),
        "b2c43bb8ae50404c7d4d62cca40fd1bc6084767025bb17c3847bca7db826a721",
        "cache_key_compile_rlib_with_deps golden value changed! This invalidates all user caches. \
         If intentional, update this value."
    );
}

#[vx_test]
async fn golden_cache_key_compile_bin_with_deps() {
    let db = test_db();
    let (toolchain, config) = setup_golden_rust(&db);

    let crate_info = RustCrate::new(
        &db,
        "crate-abc123".to_string(),
        "my_app".to_string(),
        "2021".to_string(),
        "bin".to_string(),
        "src/main.rs".to_string(),
        golden_closure_hash(),
    )
    .unwrap();

    let dep_hashes = vec![
        ("log".to_string(), Blake3Hash::from_bytes(b"log-rlib-hash")),
        (
            "serde".to_string(),
            Blake3Hash::from_bytes(b"serde-rlib-hash"),
        ),
    ];

    let key = cache_key_compile_bin_with_deps(&db, crate_info, toolchain, config, dep_hashes)
        .await
        .unwrap();
    assert_eq!(
        key.to_hex(),
        "51c6377ddc4cbedfa5f01a82ed251ba0640f86c3d36e89e32f0206e7f7e6bc63",
        "cache_key_compile_bin_with_deps golden value changed! This invalidates all user caches. \
         If intentional, update this value."
    );
}

#[vx_test]
async fn golden_cache_key_cc_compile() {
    let db = test_db();

    ZigToolchainConfig::set(&db, "0.13.0".to_string(), "zig-golden-id".to_string()).unwrap();

    let build_key = BuildConfig::compute_key(
        "release",
        "x86_64-unknown-linux-gnu",
        "/home/user/project",
        "",
    );
    let config = BuildConfig::new(
        &db,
        build_key,
        "release".to_string(),
        "x86_64-unknown-linux-gnu".to_string(),
        "/home/user/project".to_string(),
        String::new(),
    )
    .unwrap();

    let source = CSourceFile::new(
        &db,
        "src/main.c".to_string(),
        Blake3Hash::from_bytes(b"int main() { return 0; }"),
    )
    .unwrap();

    let key = cache_key_cc_compile(&db, source, config).await.unwrap();
    assert_eq!(
        key.to_hex(),
        "aa13b9c630011d5478b31ac1908d9e65fd528d9cbac3eb5a8fc573f3045caa85",
        "cache_key_cc_compile golden value changed! This invalidates all user caches. \
         If intentional, update this value."
    );
}

#[vx_test]
async fn golden_cache_key_cc_link() {
    let db = test_db();

    ZigToolchainConfig::set(&db, "0.13.0".to_string(), "zig-golden-id".to_string()).unwrap();

    let build_key = BuildConfig::compute_key(
        "release",
        "x86_64-unknown-linux-gnu",
        "/home/user/project",
        "",
    );
    let config = BuildConfig::new(
        &db,
        build_key,
        "release".to_string(),
        "x86_64-unknown-linux-gnu".to_string(),
        "/home/user/project".to_string(),
        String::new(),
    )
    .unwrap();

    let target = CTarget::new(
        &db,
        "hello".to_string(),
        vec![
            Blake3Hash::from_bytes(b"main.o golden"),
            Blake3Hash::from_bytes(b"util.o golden"),
        ],
        vec!["src/main.c".to_string(), "src/util.c".to_string()],
    )
    .unwrap();

    let key = cache_key_cc_link(&db, target, config).await.unwrap();
    assert_eq!(
        key.to_hex(),
        "293a18ee13eb527b0de56213a002cf5cb22bc6748c99ef059362cce0411f59fb",
        "cache_key_cc_link golden value changed! This invalidates all user caches. \
         If intentional, update this value."
    );
}

#[vx_test]
async fn golden_cache_key_compile_bin_with_session() {
    let db = test_db();

    let toolchain = RustToolchain::new(
        &db,
        golden_toolchain_id(),
        golden_toolchain_manifest(),
        "x86_64-unknown-linux-gnu".to_string(),
        "x86_64-unknown-linux-gnu".to_string(),
    )
    .unwrap();

    let build_key = BuildConfig::compute_key(
        "release",
        "x86_64-unknown-linux-gnu",
        "/home/user/project",
        "test-session-42",
    );
    let config = BuildConfig::new(
        &db,
        build_key,
        "release".to_string(),
        "x86_64-unknown-linux-gnu".to_string(),
        "/home/user/project".to_string(),
        "test-session-42".to_string(),
    )
    .unwrap();

    CargoToml::set(
        &db,
        golden_cargo_hash(),
        "hello-world".to_string(),
        vx_manifest::Edition::E2021,
        "src/main.rs".to_string(),
    )
    .unwrap();
    SourceClosure::set(&db, golden_closure_hash(), vec!["src/main.rs".to_string()]).unwrap();

    let key = cache_key_compile_bin(&db, toolchain, config).await.unwrap();
    assert_eq!(
        key.to_hex(),
        "0c0fd85eb4f870b378e56e6cb02363837aff4aff80f2146d9ec0b227fc8da5c0",
        "cache_key_compile_bin (with session) golden value changed! \
         If intentional, update this value."
    );
}

// =============================================================================
// EXISTING TESTS
// =============================================================================

#[vx_test]
async fn test_discovered_deps_affects_cache_key() {
    let db = test_db();
    let config = setup_cc_config(&db);

    let source_hash = Blake3Hash::from_bytes(b"#include \"header.h\"\nint main() {}");

    // First compile: no discovered deps yet
    let source1 = CSourceFile::new(&db, "src/main.c".to_string(), source_hash).unwrap();
    let key1 = cache_key_cc_compile(&db, source1, config).await.unwrap();

    // After first compile, we discover header.h dependency
    // Set discovered deps for this translation unit
    let tu_key = "cc:src/main.c:debug:x86_64-linux-musl".to_string();
    let deps_hash = Blake3Hash::from_bytes(b"include/header.h");
    DiscoveredDeps::new(&db, tu_key, deps_hash, vec!["include/header.h".to_string()]).unwrap();

    // Second compile with same source but now with discovered deps
    let source2 = CSourceFile::new(&db, "src/main.c".to_string(), source_hash).unwrap();
    let key2 = cache_key_cc_compile(&db, source2, config).await.unwrap();

    // Cache keys should be different because discovered deps changed
    assert_ne!(key1, key2);
}
