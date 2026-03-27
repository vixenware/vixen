use crate::{BridgeCallSiteKind, BridgeLocalBindingKind, RuntimeBoundaryKind, SingleSourceBridge};

#[test]
fn bridge_carries_single_source_shape_for_canonical_like_program() {
    // Shape-only check: bodies, expr spans, refs, and typed expressions are all carried.
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )

fn main(n: U64) -> U64 (
  let x = plus_one(n);
  x + x
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);

    assert!(bridge.parse_errors.is_empty());
    assert!(bridge.resolve_errors.is_empty());
    assert!(bridge.type_errors.is_empty());
    assert!(bridge.cap_errors.is_empty());

    assert_eq!(bridge.source_id.0, 0);
    assert!(
        bridge.bodies.iter().any(|body| body.name == "plus_one"),
        "plus_one body should be present"
    );
    assert!(
        bridge.bodies.iter().any(|body| body.name == "main"),
        "main body should be present"
    );
    assert!(
        !bridge.exprs.is_empty(),
        "expression spans should be carried"
    );
    assert!(
        bridge.resolved_refs.iter().any(|r| r.name == "plus_one"),
        "resolved refs should include call target names"
    );
    assert!(
        !bridge.expr_types.is_empty(),
        "expression types should be carried from type_check"
    );
}

#[test]
fn bridge_carries_build_shaped_exec_refs_and_types() {
    // Shape-only check for the build-style pressure path: local prep + exec boundary call.
    let src = r#"
fn build(root: Path, args: Array[String], env: Map[String, String]) #fs #discover -> Tree (
  let class = executor_class("default");
  let cc = discover(class, (host) => host.which("cc"));
  exec [cc, "-c", @root/main.c, "-o", @out/main.o] where {
    observer: (proc, tree) => tree,
  }
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);

    assert!(bridge.parse_errors.is_empty());
    assert!(
        bridge.resolved_refs.iter().any(|r| r.name == "exec"),
        "bridge should carry resolved reference to exec"
    );
    assert!(
        bridge.resolved_refs.iter().any(|r| r.name == "discover"),
        "bridge should carry resolved reference to discover"
    );
    assert!(
        !bridge.expr_types.is_empty(),
        "bridge should carry expression typing for build-shaped source"
    );
}

#[test]
fn bridge_carries_parse_type_and_resolve_diagnostics() {
    // Intentionally malformed source to ensure parse/type/resolve diagnostics are carried.
    let src = r#"
fn build(root: Path, args: Array[String], env: Map[String, String]) #fs -> Tree (
  let class = executor_class("default");
  discover(class, (host) => host.which("cc"));
  exec [cc, "-c", @root/main.c, "-o", @out/main.o] where {
    observer: (proc, tree) => tree,
  };
  broken(
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);

    assert!(
        !bridge.parse_errors.is_empty(),
        "parse diagnostics should be carried"
    );
    assert!(
        !bridge.type_errors.is_empty() || !bridge.resolve_errors.is_empty(),
        "type or resolve diagnostics should be carried"
    );
}

#[test]
fn bridge_carries_capability_diagnostics() {
    // Valid parse + resolution shape, but capability violation in an exec observer.
    let src = r#"
fn main() {
  exec(["gcc"]) where {
    observer: (proc, tree) => {
      discover(executor_class("default"), (host) => host.which("gcc"));
      []
    },
  }
}
"#;

    let bridge = SingleSourceBridge::from_source_text(src);

    assert!(bridge.parse_errors.is_empty());
    assert!(
        !bridge.cap_errors.is_empty(),
        "capability diagnostics should be carried"
    );
    assert!(
        bridge.type_errors.is_empty() || !bridge.cap_errors.is_empty(),
        "bridge should still carry cap diagnostics even when typing is otherwise acceptable"
    );
}

#[test]
fn bridge_indexes_repeated_local_binding_use() {
    let src = r#"
fn main(n: U64) -> U64 (
  let x = n + 1;
  x + x
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);

    let x_binding = bridge
        .local_bindings
        .iter()
        .find(|binding| binding.name == "x" && binding.kind == BridgeLocalBindingKind::Let)
        .expect("x let binding should be indexed");
    let x_uses = bridge
        .local_uses
        .iter()
        .filter(|usage| usage.binding == x_binding.id)
        .count();
    assert_eq!(x_uses, 2, "x should have two indexed uses in x + x");
}

#[test]
fn bridge_indexes_single_use_local_binding() {
    let src = r#"
fn main(n: U64) -> U64 (
  let x = n + 1;
  x + 1
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);

    let x_binding = bridge
        .local_bindings
        .iter()
        .find(|binding| binding.name == "x" && binding.kind == BridgeLocalBindingKind::Let)
        .expect("x let binding should be indexed");
    let x_uses = bridge
        .local_uses
        .iter()
        .filter(|usage| usage.binding == x_binding.id)
        .count();
    assert_eq!(x_uses, 1, "x should have one indexed use");
}

#[test]
fn bridge_indexes_parameter_references_in_top_level_body() {
    let src = r#"
fn main(n: U64) -> U64 (
  n + 1
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);

    let n_binding = bridge
        .local_bindings
        .iter()
        .find(|binding| binding.name == "n" && binding.kind == BridgeLocalBindingKind::Parameter)
        .expect("parameter n should be indexed");
    assert!(
        bridge
            .local_uses
            .iter()
            .any(|usage| usage.binding == n_binding.id),
        "parameter n should have at least one indexed local use"
    );
}

#[test]
fn bridge_local_binding_use_index_is_deterministic() {
    let src = r#"
fn main(n: U64) -> U64 (
  let x = n + 1;
  x + x
)
"#;

    let bridge_a = SingleSourceBridge::from_source_text(src);
    let bridge_b = SingleSourceBridge::from_source_text(src);

    assert_eq!(bridge_a.local_bindings, bridge_b.local_bindings);
    assert_eq!(bridge_a.local_uses, bridge_b.local_uses);
}

#[test]
fn bridge_classifies_direct_named_function_call_site() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 ( plus_one(n) )
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    assert!(
        bridge
            .call_sites
            .iter()
            .any(|site| site.kind == BridgeCallSiteKind::DirectNamedFunction),
        "plus_one(n) should classify as direct named function call"
    );
}

#[test]
fn bridge_classifies_local_callable_call_site() {
    let src = r#"
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  add_n(1)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);
    assert!(
        bridge
            .call_sites
            .iter()
            .any(|site| site.kind == BridgeCallSiteKind::LocalCallableBinding),
        "add_n(1) should classify as local callable binding call"
    );
}

#[test]
fn bridge_classifies_stdlib_boundary_call_sites() {
    let src = r#"
fn build(root: Path, args: Array[String], env: Map[String, String]) #fs #discover -> Tree (
  let class = executor_class("default");
  let cc = discover(class, (host) => host.which("cc"));
  exec [cc, "-c", @root/main.c, "-o", @out/main.o] where {
    observer: (proc, tree) => tree,
  };
  stabilize(() => "w", (w) => w)
)
"#;

    let bridge = SingleSourceBridge::from_source_text(src);

    assert!(bridge.call_sites.iter().any(|site| {
        site.kind == BridgeCallSiteKind::StdlibBoundary(RuntimeBoundaryKind::Exec)
    }));
    assert!(bridge.call_sites.iter().any(|site| {
        site.kind == BridgeCallSiteKind::StdlibBoundary(RuntimeBoundaryKind::Discover)
    }));
    assert!(bridge.call_sites.iter().any(|site| {
        site.kind == BridgeCallSiteKind::StdlibBoundary(RuntimeBoundaryKind::SharedKeyed)
    }));
}

#[test]
fn bridge_call_site_classification_is_deterministic() {
    let src = r#"
fn plus_one(n: U64) -> U64 ( n + 1 )
fn main(n: U64) -> U64 (
  let add_n = (x) => x + n;
  let class = executor_class("default");
  let cc = discover(class, (host) => host.which("cc"));
  plus_one(n) + add_n(1);
  exec [cc, "-c", @root/main.c, "-o", @out/main.o] where {
    observer: (proc, tree) => tree,
  };
  stabilize(() => "w", (w) => w)
)
"#;

    let bridge_a = SingleSourceBridge::from_source_text(src);
    let bridge_b = SingleSourceBridge::from_source_text(src);
    assert_eq!(bridge_a.call_sites, bridge_b.call_sites);
}
