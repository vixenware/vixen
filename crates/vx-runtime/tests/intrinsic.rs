//! Native (intrinsic) thunks: a struct field whose value is Rust code, forced lazily
//! and memoised. This is the mechanism `Run`'s `.ok` gate (and `.out` handle) ride
//! on — the runtime hands the observer a plain struct whose fields happen to be
//! backed by native work, so field access (`run.ok`) is ordinary, but forcing it
//! runs Rust.

use std::cell::Cell;
use std::rc::Rc;

use vx_ir::item::ItemKind;
use vx_ir::{BodyId, VirArena};
use vx_runtime::compile::compile_source;
use vx_runtime::eval::{KernelMode, Outcome, Thunk, Value, eval_body};

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
fn a_native_backed_struct_field_forces_its_intrinsic() {
    let fe = compile_source(
        "struct Box { answer: U64 }\nfn read(b: Box) -> U64 ( b.answer )",
        "read",
    )
    .expect("front-end compiles");
    let entry = fn_body(&fe.program.arena, "read");

    // A `Box` whose `answer` field is a native thunk, not a Vixen expression.
    let boxed = Value::Struct(vec![(
        "answer".to_string(),
        Thunk::native(|| Ok(Value::U64(42))),
    )]);

    let outcome = eval_body(&fe.program, &fe.env, entry, vec![boxed], KernelMode::Off);
    assert!(
        matches!(outcome, Outcome::Clean(Value::U64(42))),
        "field access forces the intrinsic → 42, got {outcome:?}"
    );
}

#[test]
fn a_native_field_is_lazy_then_memoised() {
    // `pick` reads `b.answer` only on the taken branch — conditional access; `twice`
    // reads it twice — so we can see it runs at most once.
    let fe = compile_source(
        "struct Box { answer: U64 }\n\
         fn pick(b: Box, which: Bool) -> U64 ( if which ( b.answer ) else ( 0 ) )\n\
         fn twice(b: Box) -> U64 ( b.answer + b.answer )",
        "pick",
    )
    .expect("front-end compiles");

    let counted = |calls: Rc<Cell<u32>>| {
        Value::Struct(vec![(
            "answer".to_string(),
            Thunk::native(move || {
                calls.set(calls.get() + 1);
                Ok(Value::U64(7))
            }),
        )])
    };

    // Not read (which = false) → the intrinsic never runs (laziness — the same
    // reason a bomb in an untouched field stays cold).
    let pick = fn_body(&fe.program.arena, "pick");
    let c0 = Rc::new(Cell::new(0));
    let o0 = eval_body(
        &fe.program,
        &fe.env,
        pick,
        vec![counted(c0.clone()), Value::Bool(false)],
        KernelMode::Off,
    );
    assert!(matches!(o0, Outcome::Clean(Value::U64(0))));
    assert_eq!(
        c0.get(),
        0,
        "intrinsic must not run when its field isn't read"
    );

    // Read twice → the intrinsic runs exactly once (memoised).
    let twice = fn_body(&fe.program.arena, "twice");
    let c1 = Rc::new(Cell::new(0));
    let o1 = eval_body(
        &fe.program,
        &fe.env,
        twice,
        vec![counted(c1.clone())],
        KernelMode::Off,
    );
    assert!(
        matches!(o1, Outcome::Clean(Value::U64(14))),
        "7 + 7 = 14, got {o1:?}"
    );
    assert_eq!(c1.get(), 1, "intrinsic runs exactly once, memoised");
}
