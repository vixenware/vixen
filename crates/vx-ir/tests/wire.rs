//! The VIR is shippable.
//!
//! A real compiled arena round-trips through `facet-postcard` — the wire format
//! vox uses — so an observer closure and the program it references can travel to
//! a remote executor and come back byte-identical. This is the foundation under
//! the demand-pull resolve channel: if the IR didn't serialize faithfully, none
//! of the orchestrator↔executor split would hold.

use vx_ir::VirArena;
use vx_ir::build::VirBuilder;
use vx_parser::{SourceId, parse_source_file};

/// Parse + lower a source string into a VIR arena, the way the front-end does.
fn arena_of(source: &str) -> VirArena {
    let parsed = parse_source_file(source, SourceId(0));
    let file = parsed.tree();
    VirBuilder::build_program(&[(SourceId(0), file)]).arena
}

#[test]
fn vir_arena_round_trips_through_postcard() {
    // A program touching a broad slice of the node universe: struct + enum items
    // (FieldDecl / VariantDecl), a function with a `match` (MatchExpr / literal +
    // wildcard patterns), and another with let-chains, a string interpolation,
    // field access, a closure (a nested Body), and a call.
    let source = r#"
struct Pair { a: U64, b: U64 }

enum Choice { Left, Right }

fn classify(n: U64) -> U64 (
    match n (
        0 => 100,
        _ => n,
    )
)

fn run(p: Pair) -> U64 (
    let sum = p.a + p.b;
    let label = `pair sums to {sum}`;
    let double = (x) => x * 2;
    double(sum)
)
"#;

    let arena = arena_of(source);

    // We actually built something with breadth — not an all-Error arena.
    assert!(
        arena.exprs.len() > 8,
        "expected a non-trivial arena, got {} exprs",
        arena.exprs.len()
    );
    assert_eq!(arena.items.len(), 4, "struct + enum + two fns");
    assert!(arena.bodies.len() >= 3, "two fns + the closure");

    // Ship it over the wire.
    let bytes = facet_postcard::to_vec(&arena).expect("VIR arena serializes");
    let back: VirArena = facet_postcard::from_slice(&bytes).expect("VIR arena deserializes");

    // Round-trip fidelity: the deserialized arena re-serializes to the same bytes.
    // This program has no imports/grammars, so the arena's HashMaps are empty and
    // postcard output is deterministic — byte-equality is a sound oracle here.
    let bytes2 = facet_postcard::to_vec(&back).expect("re-serializes");
    assert_eq!(bytes, bytes2, "VIR arena did not round-trip byte-for-byte");

    // Every node table survived the trip.
    assert_eq!(arena.exprs.len(), back.exprs.len(), "exprs");
    assert_eq!(arena.stmts.len(), back.stmts.len(), "stmts");
    assert_eq!(arena.patterns.len(), back.patterns.len(), "patterns");
    assert_eq!(arena.bindings.len(), back.bindings.len(), "bindings");
    assert_eq!(arena.bodies.len(), back.bodies.len(), "bodies");
    assert_eq!(arena.items.len(), back.items.len(), "items");
    assert_eq!(arena.scopes.len(), back.scopes.len(), "scopes");
    assert_eq!(arena.type_refs.len(), back.type_refs.len(), "type_refs");
    assert_eq!(
        arena.source_units.len(),
        back.source_units.len(),
        "source_units"
    );
}
