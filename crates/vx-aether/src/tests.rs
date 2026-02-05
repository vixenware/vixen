use super::*;
use tokio_test_lite::test as vx_test;
use vx_cass_proto::Blake3Hash;

/// Create a fresh database for testing
fn test_db() -> Database {
    Database::new()
}

/// Set up common build configuration for C builds
fn setup_cc_config(db: &Database) -> BuildConfig {
    let build_key = BuildConfig::compute_key("debug", "x86_64-linux-musl", "/test/workspace");
    let config = BuildConfig::new(
        db,
        build_key,
        "debug".to_string(),
        "x86_64-linux-musl".to_string(),
        "/test/workspace".to_string(),
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
    let build_key_debug = BuildConfig::compute_key("debug", "x86_64-linux-musl", "/test/workspace");
    let source_debug = CSourceFile::new(&db, "src/main.c".to_string(), source_hash).unwrap();
    let config_debug = BuildConfig::new(
        &db,
        build_key_debug,
        "debug".to_string(),
        "x86_64-linux-musl".to_string(),
        "/test/workspace".to_string(),
    )
    .unwrap();
    let key_debug = cache_key_cc_compile(&db, source_debug, config_debug)
        .await
        .unwrap();

    // Release profile
    let build_key_release =
        BuildConfig::compute_key("release", "x86_64-linux-musl", "/test/workspace");
    let config_release = BuildConfig::new(
        &db,
        build_key_release,
        "release".to_string(),
        "x86_64-linux-musl".to_string(),
        "/test/workspace".to_string(),
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
