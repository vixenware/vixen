use std::fs;
use std::path::{Path, PathBuf};

const GUARDED_CRATES: &[&str] = &["vx-runtime-core", "vx-runtime-driver"];

const FORBIDDEN_NEEDLES: &[&str] = &[
    "vx_ir",
    "vx-ir",
    "ExprKind",
    "ExprId",
    "StmtKind",
    "StmtId",
    "PatternKind",
    "PatternId",
    "VirProgram",
    "BodyId",
    "ItemKind",
    "node_exprs",
    "exprmap",
    "eval_inner",
    "eval_expr",
    "walk_expr",
    "legacy_eval",
    "fn force",
    ".force(",
];

#[test]
fn execution_crates_stay_source_blind() {
    let root = workspace_root();
    let mut failures = Vec::new();

    for krate in GUARDED_CRATES {
        let crate_dir = root.join("crates").join(krate);
        for path in guarded_files(&crate_dir) {
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));

            for needle in FORBIDDEN_NEEDLES {
                if text.contains(needle) {
                    failures.push(format!(
                        "{} contains forbidden runtime source affordance `{needle}`",
                        path.strip_prefix(&root).unwrap_or(&path).display()
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "execution crates must remain source-blind:\n{}",
        failures.join("\n")
    );
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("runtime crate lives under crates/")
        .to_path_buf()
}

fn guarded_files(crate_dir: &Path) -> Vec<PathBuf> {
    let mut files = vec![crate_dir.join("Cargo.toml")];
    collect_rs_files(&crate_dir.join("src"), &mut files);
    files
}

fn collect_rs_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|err| panic!("failed to read {}: {err}", dir.display()));

    for entry in entries {
        let entry = entry.unwrap_or_else(|err| panic!("failed to read directory entry: {err}"));
        let path = entry.path();
        let file_type = entry
            .file_type()
            .unwrap_or_else(|err| panic!("failed to inspect {}: {err}", path.display()));

        if file_type.is_dir() {
            collect_rs_files(&path, files);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}
