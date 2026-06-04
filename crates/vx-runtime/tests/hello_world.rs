//! Hello-world, end to end — the whole stack in one run.
//!
//! A build requests a `Cc` capability with `where`; the local control plane matches
//! the structured predicate against an advertised provider (the host's real C
//! compiler) and **injects** it (`r[ability.request]`/`r[ability.match]`); the
//! resulting backtick **invocation** compiles a `hello.c` to an object file
//! (`r[exec.invocation]`) — forcing the invocation runs it on a strand, and the
//! produced output tree crosses back, from which `out.file("hello.o")` pulls the
//! object's bytes over vox.
//!
//! So this exercises, together: `where` → injection (Phase 3), forced-on-demand exec
//! with the default observer (Phase 2), and the Tool/Command/Invocation values
//! (Phase 2) — driving the host toolchain to actually compile C.

use std::rc::Rc;

use vx_ir::item::ItemKind;
use vx_runtime::ability::ProviderRegistry;
use vx_runtime::compile::compile_source;
use vx_runtime::eval::{
    KernelMode, Outcome as EvalOutcome, Thunk, Value, eval_body_with_exec_and_providers,
    grammar_coord_value,
};
use vx_runtime::observe::local_observer;
use vx_runtime::strand::{Outcome, ResourceId, Scheduler};

/// The host's C compiler, if any (PATH lookup). The integration needs a real one.
fn find_cc() -> Option<String> {
    ["cc", "clang", "gcc"]
        .into_iter()
        .find_map(vx_runtime::host::which)
}

/// A `Cc` provider value (`r[ability.advertise]`): its `command` (the cc grammar
/// coordinate), the host `binary`, and the probed `version` attribute the request's
/// constraint matches on.
fn cc_provider(binary: &str, version: u64) -> Value {
    Value::Struct(vec![
        (
            "command".to_string(),
            Thunk::done(grammar_coord_value("@vixen/grammar-cc", "cc")),
        ),
        (
            "binary".to_string(),
            Thunk::done(Value::Str(Rc::from(binary))),
        ),
        ("version".to_string(), Thunk::done(Value::U64(version))),
    ])
}

#[test]
fn hello_world_compiles_through_a_requested_toolchain() {
    let Some(cc) = find_cc() else {
        // No host C compiler to integrate against — the run-the-real-compiler test
        // can't say anything. (A hermetic toolchain provider is the acquire piece.)
        return;
    };

    // A hello.c the compiler reads by absolute path (VFS input mounting is its own
    // piece). A unique temp dir, pid-keyed — no wall clock.
    let dir = std::env::temp_dir().join(format!("vx-hello-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let hello = dir.join("hello.c");
    std::fs::write(&hello, "int main(void) { return 0; }\n").expect("write hello.c");
    let hello_path = hello.to_str().expect("utf-8 path").to_string();

    // The build: request a Cc, then compile hello.c with it. The leading `$cc` of the
    // invocation is the injected tool; `-c ${hello} -o hello.o` are its arguments.
    let fe = compile_source(
        "struct Cc { version: U64 }\n\
         fn t(hello: String) -> Blob ( \
           let cc = Cc where (version >= 1); \
           let obj = `$cc -c ${hello} -o hello.o`; \
           obj.file(\"hello.o\") )",
        "t",
    )
    .expect("hello-world program compiles");
    let entry = fe
        .program
        .arena
        .items
        .iter()
        .find_map(|it| match &it.kind {
            ItemKind::Function(f) if f.name == "t" => Some(f.body),
            _ => None,
        })
        .expect("fn t");

    let s = Scheduler::new(&[4]);
    let client = s.block_on(local_observer());
    let cid = s.register_observe_client(client);

    let program = fe.program;
    let env = fe.env;
    let strand = s.spawn(ResourceId(0), move || {
        // Advertise the host compiler as a `Cc`; `Cc where (version >= 1)` injects it.
        let mut providers = ProviderRegistry::new();
        providers.advertise("Cc", cc_provider(&cc, 1));
        let hello = Value::Str(Rc::from(hello_path.as_str()));
        match eval_body_with_exec_and_providers(
            &program,
            &env,
            entry,
            vec![hello],
            KernelMode::Off,
            cid,
            providers,
        ) {
            // The object's bytes crossed back (rendered lossily as a `Str`); a real
            // compile produces a non-empty object file.
            EvalOutcome::Clean(Value::Str(bytes)) if !bytes.is_empty() => Outcome::Clean,
            EvalOutcome::Clean(other) => {
                Outcome::Poisoned(format!("expected non-empty object bytes, got {other:?}"))
            }
            EvalOutcome::Poisoned { reason, .. } => Outcome::Poisoned(reason),
        }
    });
    s.demand(strand);
    s.run();
    let outcome = s.outcome(strand);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(
        outcome,
        Some(Outcome::Clean),
        "the requested cc compiles hello.c → a non-empty hello.o crosses back"
    );
}
