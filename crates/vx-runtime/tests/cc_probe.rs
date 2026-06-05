//! The cc capability probe — step 3 of commands/grammars-are-values.
//!
//! `probe(host) -> Option[CToolchain]` resolves `cc` on `$PATH`, runs
//! `cc --version` (which *fingerprints* the binary — `host.exec` implies
//! `host.hash`), and bundles the `@vixen/grammar-cc` grammar coordinate + the
//! resolved path + that fingerprint into a `CToolchain { cc: Command }`. The
//! Command is therefore **content-identified**, not path-keyed.
//!
//! Driven in-process with a local `Host` — the daemon/advertisement layer is
//! deferred (a local probe stands in), and cross-package import is still unbuilt,
//! so `CToolchain` + `probe` ride as inline `.vx`.

use vx_ir::item::ItemKind;
use vx_package::CompositeResolver;
use vx_runtime::compile::{NoopObserver, semantic_front_end};
use vx_runtime::eval::{KernelMode, Outcome, Value, eval_body};
use vx_runtime::host::local_host_value;

/// A capability advertises its commands. The probe `which`es `cc`, runs it to read
/// its version (fingerprinting it in the same call), and bundles a `cc` Command.
/// Accessor fns project scalars out so the in-process eval forces them.
const PROBE: &str = "\
struct CToolchain { cc: Command }\n\
fn build(host: Host) -> CToolchain (\n\
  let g = import \"@vixen/grammar-cc\";\n\
  let cc_path = host.which(\"cc\");\n\
  let out = host.exec([cc_path, \"--version\"]);\n\
  CToolchain { cc: { grammar: g.cc, binary: cc_path, fingerprint: out.fingerprint } }\n\
)\n\
fn probe(host: Host) -> Option[CToolchain] ( Some(build(host)) )\n\
fn cc_binary(host: Host) -> String ( build(host).cc.binary )\n\
fn cc_fingerprint(host: Host) -> String ( build(host).cc.fingerprint )\n\
fn cc_grammar(host: Host) -> String ( build(host).cc.grammar.name )\n";

/// Compile `PROBE`, run the named fn with a local `Host`, return the outcome.
fn eval_fn(name: &str) -> Outcome {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages");
    let resolver = CompositeResolver::new(root);
    let fe = semantic_front_end(PROBE, &resolver, &mut NoopObserver).expect("front-end compiles");
    let body = fe
        .program
        .arena
        .items
        .iter()
        .find_map(|it| match &it.kind {
            ItemKind::Function(f) if f.name == name => Some(f.body),
            _ => None,
        })
        .unwrap_or_else(|| panic!("fn {name}"));
    eval_body(
        &fe.program,
        &fe.env,
        body,
        vec![local_host_value()],
        KernelMode::GraphOnly,
    )
}

#[test]
fn cc_probe_builds_a_content_identified_ctoolchain() {
    // cc is present on a dev machine → the probe yields `Some(CToolchain)`.
    match eval_fn("probe") {
        Outcome::Clean(Value::Variant { tag, .. }) => {
            assert_eq!(tag, "Some", "cc present → Some(CToolchain)")
        }
        other => panic!("expected Some(CToolchain), got {other:?}"),
    }

    // The bundled command carries the cc grammar coordinate…
    match eval_fn("cc_grammar") {
        Outcome::Clean(Value::Str(s)) => {
            assert_eq!(&*s, "cc", "the command carries the cc grammar")
        }
        other => panic!("expected grammar name `cc`, got {other:?}"),
    }
    // …the resolved binary path…
    match eval_fn("cc_binary") {
        Outcome::Clean(Value::Str(s)) => {
            assert!(
                s.contains("cc"),
                "binary is the resolved cc path, got {s:?}"
            )
        }
        other => panic!("expected the cc path, got {other:?}"),
    }
    // …and the fingerprint from having run it (exec implies hash).
    match eval_fn("cc_fingerprint") {
        Outcome::Clean(Value::Str(s)) => {
            assert!(
                s.starts_with("blake3:"),
                "cc is content-identified, got {s:?}"
            )
        }
        other => panic!("expected a blake3 fingerprint, got {other:?}"),
    }
}
