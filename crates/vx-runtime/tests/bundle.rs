//! The exec bundle is **complete**: walking an observer body collects everything
//! it can reach — across *resolution* edges (a call pulls in the callee's body), not
//! just the syntax tree — plus the transitive type closure. A missed node would
//! silently break the mirror, so this is the load-bearing test.

use vx_ir::item::ItemKind;
use vx_ir::{BodyId, VirArena, reachable};
use vx_runtime::compile::compile_source;
use vx_types::{Ty, TypeDef};

const SRC: &str = r#"
struct Stat { n: U64 }

fn helper(n: U64) -> U64 ( n + 1 )

fn observe(s: Stat) -> U64 ( helper(s.n) )
"#;

fn fn_body(arena: &VirArena, name: &str) -> BodyId {
    arena
        .items
        .iter()
        .find_map(|it| match &it.kind {
            ItemKind::Function(f) if f.name == name => Some(f.body),
            _ => None,
        })
        .unwrap_or_else(|| panic!("function `{name}` not found"))
}

#[test]
fn bundle_collects_the_reachable_closure_across_resolution_edges() {
    let fe = compile_source(SRC, "observe").expect("front-end compiles");
    let arena = &fe.program.arena;
    let env = &fe.env;

    let entry = fn_body(arena, "observe");
    let helper = fn_body(arena, "helper");

    let bundle = reachable(arena, env, entry);

    // The entry body travels.
    assert!(
        bundle.bodies.iter().any(|(id, _)| *id == entry),
        "the entry body must be in the bundle"
    );
    // `helper`'s body is reached through the call's NameRef→Item resolution edge —
    // it is NOT a syntactic child of `observe`'s expression tree.
    assert!(
        bundle.bodies.iter().any(|(id, _)| *id == helper),
        "the called function's body must be reached via resolution, not just syntax"
    );
    assert!(
        bundle
            .items
            .iter()
            .any(|(_, it)| matches!(&it.kind, ItemKind::Function(f) if f.name == "helper")),
        "the helper item must travel"
    );
    // The `Stat` struct's type + type def are reached via the parameter's type.
    assert!(
        bundle
            .types
            .iter()
            .any(|(_, ty)| matches!(ty, Ty::Struct(_))),
        "the Stat struct type must be in the type closure"
    );
    assert!(
        bundle
            .type_defs
            .iter()
            .any(|(_, d)| matches!(d, TypeDef::Struct(_))),
        "the Stat struct def must travel"
    );
}

/// The mirror installs the bundle in the original id space and reads every collected
/// node back identically — `arena[id]` and `env.get_ty(id)` match the orchestrator's.
#[test]
fn install_reconstructs_every_collected_node_in_place() {
    let fe = compile_source(SRC, "observe").expect("front-end compiles");
    let entry = fn_body(&fe.program.arena, "observe");
    let bundle = reachable(&fe.program.arena, &fe.env, entry);

    let (prog, env) = bundle.install();

    // The entry body lands at its verbatim id.
    assert_eq!(
        prog.arena[entry].root_expr,
        fe.program.arena[entry].root_expr
    );

    // Every collected expr/body reconstructs in place (Debug-equal — nodes are Clone,
    // not PartialEq, so we compare their rendered form).
    for (id, _) in &bundle.exprs {
        assert_eq!(
            format!("{:?}", prog.arena[*id]),
            format!("{:?}", fe.program.arena[*id]),
            "expr {id:?} reconstructs in place"
        );
    }
    for (id, _) in &bundle.bodies {
        assert_eq!(
            format!("{:?}", prog.arena[*id]),
            format!("{:?}", fe.program.arena[*id]),
            "body {id:?} reconstructs in place"
        );
    }
    // Every collected type resolves to the same structural `Ty` under the same id.
    for (id, ty) in &bundle.types {
        assert_eq!(env.get_ty(*id), ty, "type {id:?} installed verbatim");
        assert_eq!(
            env.get_ty(*id),
            fe.env.get_ty(*id),
            "type {id:?} matches origin"
        );
    }
}
