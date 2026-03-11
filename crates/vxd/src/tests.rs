use super::*;
use std::sync::{Arc, Mutex, MutexGuard};

use arborium::advanced::{CompiledGrammar, GrammarConfig};
use arborium::{GrammarStore, Highlighter};
use arborium_highlight::spans_to_themed;
use arborium_theme::theme::builtin;
use ariadne::{Color as AriadneColor, Config as AriadneConfig, Label, Report, ReportKind, Source};
use camino::Utf8Path;
use concolor::ColorChoice;
use std::fs;
use std::net::TcpListener;
use tempfile::TempDir;
use tokio_test_lite::test as vx_test;
use vx_io::spawn::{ServiceBuilder, ServiceHandle};
use vx_report::{GraphEdge, GraphNode, GraphTopology};
use vx_store_proto::chunking::{prepare_blob, store_prepared_blob};
use vx_store_proto::{Blake3Hash, BlobRef, ContentHash, DirEntryKind, StoreClient};

/// Create a fresh database for testing
fn test_db() -> Database {
    Database::new()
}

static COLOR_OVERRIDE_LOCK: Mutex<()> = Mutex::new(());

struct ColorOverrideGuard {
    _guard: MutexGuard<'static, ()>,
}

impl ColorOverrideGuard {
    fn always_ansi() -> Self {
        let guard = COLOR_OVERRIDE_LOCK.lock().expect("color override lock");
        concolor::set(ColorChoice::AlwaysAnsi);
        Self { _guard: guard }
    }
}

impl Drop for ColorOverrideGuard {
    fn drop(&mut self) {
        concolor::set(ColorChoice::Auto);
    }
}

fn vixen_grammar_store() -> Result<Arc<GrammarStore>, Box<dyn std::error::Error>> {
    let grammar = Arc::new(CompiledGrammar::new(GrammarConfig {
        language: tree_sitter_vixen::language().into(),
        highlights_query: tree_sitter_vixen::HIGHLIGHTS_QUERY,
        injections_query: tree_sitter_vixen::INJECTIONS_QUERY,
        locals_query: tree_sitter_vixen::LOCALS_QUERY,
    })?);

    let store = Arc::new(GrammarStore::new());
    store.insert("vixen", grammar);
    Ok(store)
}

fn line_order(source: &str, offset: usize) -> i32 {
    Source::from(source)
        .get_offset_line(offset)
        .map(|(_, line, _)| line as i32)
        .expect("offset should map to a source line")
}

#[test]
fn test_sanitize_id_component_replaces_non_identifier_chars() {
    assert_eq!(
        crate::service::sanitize_id_component("hello/world::crate@1.2.3"),
        "hello_world__crate_1_2_3"
    );
}

#[test]
fn test_legacy_compile_node_id_format() {
    assert_eq!(
        crate::service::legacy_compile_node_id("my-crate", "bin"),
        "compile-bin:my-crate"
    );
}

#[test]
fn test_rule_plan_parity_detects_missing_focus_nodes() {
    let legacy = GraphTopology {
        nodes: vec![GraphNode {
            index: 0,
            node_id: "compile-bin:app".to_string(),
            kind: "rust.compile_bin".to_string(),
            label: "compile app".to_string(),
        }],
        edges: vec![],
    };
    let rule_plan = GraphTopology {
        nodes: vec![],
        edges: vec![],
    };

    let diff = crate::service::compare_rule_plan_parity(&legacy, &rule_plan);
    assert_eq!(diff.legacy_focus_count, 1);
    assert_eq!(diff.rule_plan_focus_count, 0);
    assert_eq!(
        diff.missing_in_rule_plan,
        vec!["compile-bin:app".to_string()]
    );
    assert!(diff.extra_in_rule_plan.is_empty());
}

#[test]
fn test_rule_plan_parity_ignores_non_focus_nodes() {
    let legacy = GraphTopology {
        nodes: vec![GraphNode {
            index: 0,
            node_id: "download toolchain Rustc".to_string(),
            kind: "toolchain.component.download".to_string(),
            label: "download toolchain Rustc".to_string(),
        }],
        edges: vec![GraphEdge {
            from: 0,
            to: 0,
            kind: Some("always".to_string()),
        }],
    };
    let rule_plan = GraphTopology {
        nodes: vec![GraphNode {
            index: 0,
            node_id: "planner-action:n_action".to_string(),
            kind: "planner.action".to_string(),
            label: "planner action".to_string(),
        }],
        edges: vec![],
    };

    let diff = crate::service::compare_rule_plan_parity(&legacy, &rule_plan);
    assert_eq!(diff.legacy_focus_count, 0);
    assert_eq!(diff.rule_plan_focus_count, 0);
    assert!(diff.missing_in_rule_plan.is_empty());
    assert!(diff.extra_in_rule_plan.is_empty());
}

#[test]
fn arborium_can_highlight_vixen_via_external_grammar_crate()
-> Result<(), Box<dyn std::error::Error>> {
    let mut highlighter = Highlighter::with_store(vixen_grammar_store()?);
    let html = highlighter.highlight(
        "vixen",
        r#"fn build(ws: Path) -> Tree (
  let src_tree = Tree(ws / @zoo/lua-src);
  src_tree
)"#,
    )?;
    println!("{html}");

    assert!(html.contains("<a-"), "{html}");
    assert!(html.contains("build"), "{html}");
    assert!(html.contains("lua-src"), "{html}");

    Ok(())
}

#[test]
fn ariadne_can_render_vixen_source_with_arborium_color_labels()
-> Result<(), Box<dyn std::error::Error>> {
    let _color = ColorOverrideGuard::always_ansi();
    let source = r#"fn build(ws: Path) -> Tree (
  let src_tree = Tree(ws / @zoo/lua-src);
  src_tree
)"#;
    let filename = "build.vx";
    let semantic_text = "@zoo/lua-src";
    let semantic_start = source.find(semantic_text).expect("semantic label start");
    let semantic_range = semantic_start..semantic_start + semantic_text.len();

    let mut highlighter = Highlighter::with_store(vixen_grammar_store()?);
    let raw_spans = highlighter.highlight_spans("vixen", source)?;
    let themed_spans = spans_to_themed(raw_spans);
    let theme = builtin::catppuccin_mocha();
    let mut syntax_label_count = 0usize;

    let mut report = Report::build(ReportKind::Error, (filename, semantic_range.clone()))
        .with_config(AriadneConfig::default().with_color(true))
        .with_message("build input not found");

    for span in themed_spans {
        if span.start >= span.end {
            continue;
        }

        let Some(style) = theme.style(span.theme_index) else {
            continue;
        };
        let Some(fg) = style.fg else {
            continue;
        };

        syntax_label_count += 1;
        report = report.with_label(
            Label::new((filename, span.start as usize..span.end as usize))
                .with_order(line_order(source, span.start as usize))
                .with_priority(0)
                .with_color(AriadneColor::Rgb(fg.r, fg.g, fg.b)),
        );
    }

    assert!(syntax_label_count > 4, "expected syntax-colored labels");

    let mut out = Vec::new();
    report
        .with_label(
            Label::new((filename, semantic_range))
                .with_order(line_order(source, semantic_start))
                .with_priority(10)
                .with_color(AriadneColor::Red)
                .with_message("this path does not exist in the current workspace"),
        )
        .with_note("resolved path: /workspace/zoo/lua-src")
        .with_note("workspace root: /workspace")
        .with_note("os error: No such file or directory")
        .finish()
        .write((filename, Source::from(source)), &mut out)?;

    assert!(
        out.contains(&b'\x1b'),
        "expected ANSI escape codes in forced-color output: {out:?}"
    );

    let rendered = String::from_utf8(out)?;
    println!("{rendered}");
    assert!(rendered.contains("build input not found"), "{rendered}");
    assert!(
        rendered.contains("this path does not exist in the current workspace"),
        "{rendered}"
    );
    assert!(rendered.contains("lua-src"), "{rendered}");
    assert!(
        rendered.contains("resolved path: /workspace/zoo/lua-src"),
        "{rendered}"
    );
    assert_eq!(rendered.matches("build.vx:").count(), 1, "{rendered}");

    Ok(())
}

#[test]
fn test_rule_plan_edge_parity_compares_focus_deps_with_two_hop_bridge() {
    // Legacy: compile(app) depends directly on compile(dep) and registry acquire.
    let legacy = GraphTopology {
        nodes: vec![
            GraphNode {
                index: 0,
                node_id: "compile-bin:app".to_string(),
                kind: "rust.compile_bin".to_string(),
                label: "compile app".to_string(),
            },
            GraphNode {
                index: 1,
                node_id: "compile-lib:dep".to_string(),
                kind: "rust.compile_rlib".to_string(),
                label: "compile dep".to_string(),
            },
            GraphNode {
                index: 2,
                node_id: "acquire-registry:serde:1.0.0".to_string(),
                kind: "registry.acquire".to_string(),
                label: "acquire serde".to_string(),
            },
        ],
        edges: vec![
            GraphEdge {
                from: 0,
                to: 1,
                kind: Some("always".to_string()),
            },
            GraphEdge {
                from: 0,
                to: 2,
                kind: Some("always".to_string()),
            },
        ],
    };

    // RulePlan-lowered: compile(app) depends on dep_artifact (non-focus) and registry acquire.
    // dep_artifact depends on compile(dep). This should still count as a focus dep via 2-hop walk.
    let rule_plan = GraphTopology {
        nodes: vec![
            GraphNode {
                index: 0,
                node_id: "compile-bin:app".to_string(),
                kind: "rust.compile_bin".to_string(),
                label: "compile app".to_string(),
            },
            GraphNode {
                index: 1,
                node_id: "artifact:dep".to_string(),
                kind: "planner.artifact".to_string(),
                label: "artifact dep".to_string(),
            },
            GraphNode {
                index: 2,
                node_id: "compile-lib:dep".to_string(),
                kind: "rust.compile_rlib".to_string(),
                label: "compile dep".to_string(),
            },
            GraphNode {
                index: 3,
                node_id: "acquire-registry:serde:1.0.0".to_string(),
                kind: "registry.acquire".to_string(),
                label: "acquire serde".to_string(),
            },
        ],
        edges: vec![
            GraphEdge {
                from: 0,
                to: 1,
                kind: Some("ifchanged".to_string()),
            },
            GraphEdge {
                from: 1,
                to: 2,
                kind: Some("ifwritten".to_string()),
            },
            GraphEdge {
                from: 0,
                to: 3,
                kind: Some("ifchanged".to_string()),
            },
        ],
    };

    let diff = crate::service::compare_rule_plan_edge_parity(&legacy, &rule_plan);
    assert!(diff.missing_in_rule_plan.is_empty(), "{diff:?}");
    assert!(diff.extra_in_rule_plan.is_empty(), "{diff:?}");
}

#[test]
fn test_rule_plan_execution_selects_crate_subset_and_closure() {
    use std::collections::{BTreeMap, HashMap, HashSet};
    use vx_manifest::Edition;
    use vx_report::{RuleId, RulePlan, RulePlanNode, RulePlanNodeId, RulePlanNodeKind};
    use vx_rs::crate_graph::{CrateGraph, CrateId, CrateNode, CrateSource, CrateType, DepEdge};

    let workspace_root = camino::Utf8PathBuf::from("/workspace");

    let dep_source = CrateSource::Path {
        workspace_rel: camino::Utf8PathBuf::from("dep"),
    };
    let dep_id = CrateId::from_source(&dep_source, "dep");
    let dep_node = CrateNode {
        id: dep_id,
        package_name: "dep".to_string(),
        crate_name: "dep".to_string(),
        source: dep_source,
        workspace_rel: camino::Utf8PathBuf::from("dep"),
        crate_root_rel: camino::Utf8PathBuf::from("dep/src/lib.rs"),
        edition: Edition::E2021,
        crate_type: CrateType::Lib,
        deps: vec![],
        build_script_rel: None,
        build_script_output: None,
    };

    let app_source = CrateSource::Path {
        workspace_rel: camino::Utf8PathBuf::from("app"),
    };
    let app_id = CrateId::from_source(&app_source, "app");
    let app_node = CrateNode {
        id: app_id,
        package_name: "app".to_string(),
        crate_name: "app".to_string(),
        source: app_source,
        workspace_rel: camino::Utf8PathBuf::from("app"),
        crate_root_rel: camino::Utf8PathBuf::from("app/src/main.rs"),
        edition: Edition::E2021,
        crate_type: CrateType::Bin,
        deps: vec![DepEdge {
            extern_name: "dep".to_string(),
            crate_id: dep_id,
        }],
        build_script_rel: None,
        build_script_output: None,
    };

    let graph = CrateGraph {
        workspace_root: workspace_root.clone(),
        nodes: HashMap::from([(dep_id, dep_node), (app_id, app_node)]),
        topo_order: vec![dep_id, app_id],
        root_crate: app_id,
        registry_crates: vec![],
        reachable_packages: None,
    };

    let provenance = vx_report::RulePlanProvenance {
        rule_id: RuleId::from("rule.test".to_string()),
        source_id: vx_report::RulePlanSourceId::from("src".to_string()),
        span: vx_report::RulePlanSpan {
            line: 1,
            column: 1,
            end_line: 1,
            end_column: 1,
        },
        comment: None,
        metadata: {
            let mut m = BTreeMap::new();
            m.insert("crate_name".to_string(), "app".to_string());
            m.insert("crate_type".to_string(), "bin".to_string());
            m
        },
    };

    let plan = RulePlan {
        sources: vec![],
        nodes: vec![RulePlanNode {
            id: RulePlanNodeId::from("n_action_app".to_string()),
            kind: RulePlanNodeKind::Action,
            label: "compile-bin:app".to_string(),
            provenance,
        }],
        edges: vec![],
        facts: vec![],
    };

    let selected = crate::service::selected_crates_from_rule_plan(&plan, &graph).unwrap();
    assert_eq!(selected.len(), 1);
    assert!(selected.contains(&app_id));

    let closure = crate::service::transitive_closure_crates(&graph, &selected);
    assert_eq!(closure, HashSet::from([app_id, dep_id]));
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

#[vx_test]
async fn test_store_backed_build_vx_trees_materialize_from_cas() {
    let env = StoreTestEnv::new().await;

    let bytes = b"hello from store";
    let prepared = prepare_blob(bytes);
    let blob_ref = store_prepared_blob(&env.client, &prepared, bytes)
        .await
        .expect("blob should store");
    let file_kind = dir_entry_kind_from_blob_ref(&blob_ref, false);
    let (root, _total_size) = crate::cas_tree::build_merkle_tree(
        &env.client,
        vec![crate::cas_tree::TreeFile {
            path: "nested/greeting.txt".to_string(),
            kind: file_kind,
        }],
    )
    .await
    .expect("tree should store");

    let output = TempDir::new().expect("failed to create output tempdir");
    let output_root = Utf8Path::from_path(output.path()).expect("tempdir should be utf-8");
    crate::service::materialize_store_tree_into_dir_async(&env.client, root, output_root)
        .await
        .expect("store-backed tree should materialize");

    let rendered = fs::read_to_string(output.path().join("nested/greeting.txt"))
        .expect("materialized file should be readable");
    assert_eq!(rendered, "hello from store");
}

#[vx_test]
#[cfg(unix)]
async fn test_store_backed_build_vx_trees_preserve_executable_bits() {
    use std::os::unix::fs::PermissionsExt;

    let env = StoreTestEnv::new().await;

    let bytes = b"#!/bin/sh\nexit 0\n";
    let prepared = prepare_blob(bytes);
    let blob_ref = store_prepared_blob(&env.client, &prepared, bytes)
        .await
        .expect("blob should store");
    let file_kind = dir_entry_kind_from_blob_ref(&blob_ref, true);
    let (root, _total_size) = crate::cas_tree::build_merkle_tree(
        &env.client,
        vec![crate::cas_tree::TreeFile {
            path: "bin/tool".to_string(),
            kind: file_kind,
        }],
    )
    .await
    .expect("tree should store");

    let output = TempDir::new().expect("failed to create output tempdir");
    let output_root = Utf8Path::from_path(output.path()).expect("tempdir should be utf-8");
    crate::service::materialize_store_tree_into_dir_async(&env.client, root, output_root)
        .await
        .expect("store-backed tree should materialize");

    let executable = output.path().join("bin/tool");
    let mode = fs::metadata(&executable)
        .expect("materialized file should stat")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o755, "expected executable mode on materialized file");
}

struct StoreTestEnv {
    _root: TempDir,
    _vx_home: TempDir,
    _service: ServiceHandle,
    client: StoreClient,
}

impl StoreTestEnv {
    async fn new() -> Self {
        let root = TempDir::new().expect("failed to create store root tempdir");
        let vx_home = TempDir::new().expect("failed to create vx_home tempdir");
        let endpoint = free_local_endpoint();
        let binary_path = resolve_vx_store_binary();
        let root_str = root.path().to_string_lossy().into_owned();

        let service = ServiceBuilder::new(binary_path, endpoint)
            .caller_name("vxd-store-materialize-tests")
            .args(&["--root", &root_str])
            .env("VX_TEST", "1")
            .timeout(std::time::Duration::from_secs(10))
            .connect()
            .await
            .expect("failed to spawn/connect vx-store");

        let virtual_handle = service
            .boot_and_connect()
            .await
            .expect("failed to boot/connect vx-store virtual connection");

        Self {
            _root: root,
            _vx_home: vx_home,
            _service: service,
            client: StoreClient::new(virtual_handle),
        }
    }
}

fn dir_entry_kind_from_blob_ref(blob_ref: &BlobRef, executable: bool) -> DirEntryKind {
    match blob_ref {
        BlobRef::Small { chunk, size } => DirEntryKind::SmallFile {
            content_hash: ContentHash(chunk.0),
            size: *size,
            executable,
        },
        BlobRef::Large {
            blob_node,
            content_hash,
            size,
        } => DirEntryKind::LargeFile {
            blob_node: *blob_node,
            content_hash: *content_hash,
            size: *size,
            executable,
        },
    }
}

fn free_local_endpoint() -> vx_io::net::Endpoint {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind ephemeral port");
    let addr = listener
        .local_addr()
        .expect("failed to read ephemeral port");
    drop(listener);
    vx_io::net::Endpoint::parse(&addr.to_string()).expect("failed to parse test endpoint")
}

fn resolve_vx_store_binary() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug/vx-store")
        .to_string_lossy()
        .into_owned()
}
