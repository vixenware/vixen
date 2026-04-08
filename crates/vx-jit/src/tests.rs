use crate::{
    StrictFragmentAbiLayout, StrictFragmentValue, compile_source_text_unified,
    execute_compiled_copying_strict_fragment_strings, execute_compiled_unmanaged_strict_fragment,
    execute_source_text_unified, lower_source_text_unified,
};

const SOURCE_ADD_U64: &str = r#"
fn add(lhs: U64, rhs: U64) -> U64 (
  lhs + rhs
)
"#;

const SOURCE_SPARSE_PREFIX: &str = r#"
fn sparse_prefix(name: String) -> String (
  let len = name.len();
  if len == 1 (
    "1"
  ) else (
    if len == 2 (
      "2"
    ) else (
      if len == 3 (
        `3/{name.slice(0..1)}`
      ) else (
        `{name.slice(0..2)}/{name.slice(2..4)}`
      )
    )
  )
)
"#;

const SOURCE_STR_LEN: &str = r#"
fn str_len(name: String) -> U64 (
  name.len()
)
"#;

const SOURCE_STR_SLICE: &str = r#"
fn str_slice(name: String, start: U64, end: U64) -> String (
  name.slice(start..end)
)
"#;

const SOURCE_ONE: &str = r#"
fn one() -> String (
  "1"
)
"#;

const SOURCE_PICK: &str = r#"
fn pick(flag: U64) -> String (
  if flag == 1 (
    "yes"
  ) else (
    "no"
  )
)
"#;

const SOURCE_JOIN: &str = r#"
fn join(a: String, b: String) -> String (
  `{a}{b}`
)
"#;

const CARGO_RESOLVE_SOURCE: &str = include_str!("../../../zoo/cargo-resolve.vx");

fn cargo_resolve_function_source(function_name: &str) -> String {
    let needle = format!("fn {function_name}(");
    let start = CARGO_RESOLVE_SOURCE
        .find(&needle)
        .unwrap_or_else(|| panic!("missing {function_name} in cargo-resolve.vx"));
    let tail = &CARGO_RESOLVE_SOURCE[start..];
    let end = tail.find("\nfn ").unwrap_or(tail.len());
    tail[..end].trim().to_owned() + "\n"
}

/// Helper: lower source through unified path, find the named function body,
/// assert it's strict, compile via Kajit, return the compiled function pointer.
fn unified_compile(source: &str, function_name: &str) -> kajit::CompiledFunction {
    let bridge = crate::legacy::SingleSourceBridge::from_source_text(source);
    let analysis = crate::legacy::build_minimal_analysis(&bridge);
    let program = crate::legacy::lower::lower_executable_program(&analysis, &bridge)
        .expect("unified lowering should succeed");

    let body = program
        .bodies
        .values()
        .find(|b| {
            matches!(&b.kernel_kind, crate::legacy::KernelKind::Strict { vtf } if vtf.name == function_name)
        })
        .unwrap_or_else(|| {
            panic!(
                "`{function_name}` should be strict. Bodies: {:?}",
                program
                    .bodies
                    .values()
                    .map(|b| format!("{:?}: {:?}", b.id, std::mem::discriminant(&b.kernel_kind)))
                    .collect::<Vec<_>>()
            )
        });

    let crate::legacy::KernelKind::Strict { vtf } = &body.kernel_kind else {
        unreachable!();
    };

    let lowerer = crate::legacy::lower_strict::StrictLowerer::new();
    let module = lowerer
        .module
        .lower_vixen_typed_function_into_module(vtf)
        .unwrap_or_else(|e| panic!("HIR lowering of `{function_name}` failed: {e:?}"));
    kajit::compile_hir_module(&module)
}

// --- Unified path: marks strict ---

#[test]
fn unified_path_marks_strict_add_as_strict() {
    let bridge = crate::legacy::SingleSourceBridge::from_source_text(SOURCE_ADD_U64);
    let analysis = crate::legacy::build_minimal_analysis(&bridge);
    let program = crate::legacy::lower::lower_executable_program(&analysis, &bridge)
        .expect("unified lowering should succeed");

    let body = program.bodies.values().next().expect("should have a body");
    assert!(
        matches!(body.kernel_kind, crate::legacy::KernelKind::Strict { .. }),
        "add(U64, U64) -> U64 should be lowered as strict, got {:?}",
        body.kernel_kind
    );
}

// --- Unified path: low-level compiled execution ---

#[test]
fn unified_executes_u64_add() {
    let compiled = unified_compile(SOURCE_ADD_U64, "add");
    let add: unsafe extern "C" fn(u64, u64) -> u64 =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    assert_eq!(unsafe { add(41, 1) }, 42);
    assert_eq!(unsafe { add(0, 0) }, 0);
    assert_eq!(unsafe { add(100, 200) }, 300);
}

#[test]
fn unified_executes_string_len() {
    let compiled = unified_compile(SOURCE_STR_LEN, "str_len");
    let str_len: unsafe extern "C" fn(u64, u64) -> u64 =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    let input = "hello";
    assert_eq!(
        unsafe { str_len(input.as_ptr() as u64, input.len() as u64) },
        5
    );
    let empty = "";
    assert_eq!(
        unsafe { str_len(empty.as_ptr() as u64, empty.len() as u64) },
        0
    );
}

#[test]
fn unified_executes_string_slice() {
    let compiled = unified_compile(SOURCE_STR_SLICE, "str_slice");
    let str_slice: unsafe extern "C" fn(u64, u64, u64, u64) -> (u64, u64) =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    let input = "hello world";
    let (ptr, len) = unsafe { str_slice(input.as_ptr() as u64, input.len() as u64, 0, 5) };
    let output = unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(ptr as *const u8, len as usize)).unwrap()
    };
    assert_eq!(output, "hello");
}

#[test]
fn unified_executes_string_literal_return() {
    let compiled = unified_compile(SOURCE_ONE, "one");
    let one: unsafe extern "C" fn() -> (u64, u64) =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    let (ptr, len) = unsafe { one() };
    let output = unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(ptr as *const u8, len as usize)).unwrap()
    };
    assert_eq!(output, "1");
}

#[test]
fn unified_executes_string_if_return() {
    let compiled = unified_compile(SOURCE_PICK, "pick");
    let pick: unsafe extern "C" fn(u64) -> (u64, u64) =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    let (ptr, len) = unsafe { pick(1) };
    let yes = unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(ptr as *const u8, len as usize)).unwrap()
    };
    assert_eq!(yes, "yes");
    let (ptr, len) = unsafe { pick(0) };
    let no = unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(ptr as *const u8, len as usize)).unwrap()
    };
    assert_eq!(no, "no");
}

#[test]
fn unified_executes_string_concat() {
    let compiled = unified_compile(SOURCE_JOIN, "join");
    let join: unsafe extern "C" fn(u64, u64, u64, u64) -> (u64, u64) =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    let a = "hello";
    let b = " world";
    let (ptr, len) = unsafe {
        join(
            a.as_ptr() as u64,
            a.len() as u64,
            b.as_ptr() as u64,
            b.len() as u64,
        )
    };
    let output = unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(ptr as *const u8, len as usize)).unwrap()
    };
    assert_eq!(output, "hello world");
    unsafe {
        std::alloc::dealloc(
            ptr as *mut u8,
            std::alloc::Layout::from_size_align(len as usize, 1).unwrap(),
        );
    }
}

#[test]
fn unified_executes_sparse_prefix() {
    let compiled = unified_compile(SOURCE_SPARSE_PREFIX, "sparse_prefix");
    let sparse_prefix: unsafe extern "C" fn(u64, u64) -> (u64, u64) =
        unsafe { core::mem::transmute(compiled.as_ptr()) };

    let input = "abc";
    let (ptr, len) = unsafe { sparse_prefix(input.as_ptr() as u64, input.len() as u64) };
    let output = unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(ptr as *const u8, len as usize)).unwrap()
    };
    assert_eq!(output, "3/a");

    let input = "serde";
    let (ptr, len) = unsafe { sparse_prefix(input.as_ptr() as u64, input.len() as u64) };
    let output = unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(ptr as *const u8, len as usize)).unwrap()
    };
    assert_eq!(output, "se/rd");
}

#[test]
fn unified_executes_registry_entry_key() {
    let source = cargo_resolve_function_source("registry_entry_key");
    let compiled = unified_compile(&source, "registry_entry_key");
    let registry_entry_key: unsafe extern "C" fn(u64, u64, u64, u64) -> (u64, u64) =
        unsafe { core::mem::transmute(compiled.as_ptr()) };
    let package_name = "serde";
    let version = "1.0.228";
    let (ptr, len) = unsafe {
        registry_entry_key(
            package_name.as_ptr() as u64,
            package_name.len() as u64,
            version.as_ptr() as u64,
            version.len() as u64,
        )
    };
    let output = unsafe {
        std::str::from_utf8(std::slice::from_raw_parts(ptr as *const u8, len as usize)).unwrap()
    };
    assert_eq!(output, "entry:registry:serde@1.0.228");
    unsafe {
        std::alloc::dealloc(
            ptr as *mut u8,
            std::alloc::Layout::from_size_align(len as usize, 1).unwrap(),
        );
    }
}

// --- High-level unified API tests ---

#[test]
fn high_level_unified_u64_add() {
    let result = unsafe {
        execute_source_text_unified(
            SOURCE_ADD_U64,
            "add",
            &[StrictFragmentValue::U64(30), StrictFragmentValue::U64(12)],
        )
    }
    .expect("unified u64 add should execute");
    assert_eq!(result, StrictFragmentValue::U64(42));
}

#[test]
fn high_level_unified_string_slice() {
    let result = unsafe {
        execute_source_text_unified(
            SOURCE_STR_SLICE,
            "str_slice",
            &[
                StrictFragmentValue::String("serde".to_owned()),
                StrictFragmentValue::U64(1),
                StrictFragmentValue::U64(4),
            ],
        )
    }
    .expect("unified str_slice should execute");
    assert_eq!(result, StrictFragmentValue::String("erd".to_owned()));
}

#[test]
fn high_level_unified_sparse_prefix() {
    let result = unsafe {
        execute_source_text_unified(
            SOURCE_SPARSE_PREFIX,
            "sparse_prefix",
            &[StrictFragmentValue::String("serde".to_owned())],
        )
    }
    .expect("unified sparse_prefix should execute");
    assert_eq!(result, StrictFragmentValue::String("se/rd".to_owned()));
}

#[test]
fn abi_layout_word_counts() {
    assert_eq!(StrictFragmentAbiLayout::U64.word_count(), 1);
    assert_eq!(StrictFragmentAbiLayout::Bool.word_count(), 1);
    assert_eq!(StrictFragmentAbiLayout::String.word_count(), 2);
}

#[test]
fn abi_bridge_u64_add() {
    let (compiled, vtf) =
        compile_source_text_unified(SOURCE_ADD_U64, "add").expect("add should compile");

    let result = unsafe {
        execute_compiled_unmanaged_strict_fragment(
            &vtf,
            &compiled,
            &[StrictFragmentValue::U64(30), StrictFragmentValue::U64(12)],
        )
    }
    .expect("abi bridge should execute add");

    assert_eq!(
        result,
        crate::CompiledStrictFragmentOutput::Value(StrictFragmentValue::U64(42))
    );
}

#[test]
fn abi_bridge_string_literal() {
    let (compiled, vtf) =
        compile_source_text_unified(SOURCE_ONE, "one").expect("one should compile");

    let result = unsafe { execute_compiled_unmanaged_strict_fragment(&vtf, &compiled, &[]) }
        .expect("abi bridge should execute one");

    let crate::CompiledStrictFragmentOutput::UnmanagedString(result) = result else {
        panic!("expected unmanaged string return");
    };
    let output = unsafe { result.to_owned_string() }.expect("result should be utf-8");
    assert_eq!(output, "1");
}

#[test]
fn abi_bridge_string_slice_copying() {
    let (compiled, vtf) = compile_source_text_unified(SOURCE_STR_SLICE, "str_slice")
        .expect("str_slice should compile");

    let result = unsafe {
        execute_compiled_copying_strict_fragment_strings(
            &vtf,
            &compiled,
            &[
                StrictFragmentValue::String("serde".to_owned()),
                StrictFragmentValue::U64(1),
                StrictFragmentValue::U64(4),
            ],
        )
    }
    .expect("abi bridge should execute str_slice");

    assert_eq!(result, StrictFragmentValue::String("erd".to_owned()));
}

#[test]
fn lower_source_text_unified_returns_vtf() {
    let lowered = lower_source_text_unified(SOURCE_ADD_U64, "add")
        .expect("add should lower through unified path");
    assert_eq!(lowered.vtf.name, "add");
    assert_eq!(lowered.vtf.params.len(), 2);
}
