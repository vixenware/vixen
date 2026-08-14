//! vixen-primitives — the concrete primitives and pure-vix stdlib the `vixen`
//! runtime installs into the `vix` language.
//!
//! `vix-core` is the bare language: it knows the *shape* of the builtin
//! primitives (their [`vix::runtime`] descriptors) but ships no implementations
//! and no prelude. This crate is the other half — the `RawPrimitive`
//! implementations behind `decode`/`fetch`/`observe`/`tree-read`, the typed
//! convenience layer they are written against, and the [`stdlib`] prelude
//! functions ([`stdlib::PRELUDE_SOURCES`]) — assembled into a runnable system by
//! `vixen-runtime`.

pub mod capability_package;
pub mod typed_primitive;

mod blob_gunzip_primitive;
mod blob_len_primitive;
mod blob_slice_primitive;
mod blob_text_primitive;
mod decode_primitive;
mod exec_primitive;
mod fetch_primitive;
mod http_origin;
mod primitive_value_decode;
mod registry_url_primitive;
mod tree_glob_primitive;
mod tree_read_primitive;
mod untar_primitive;

pub mod stdlib;

/// Single import root for the primitive modules: the core raw effect protocol
/// (`vix::runtime` — `RawPrimitive`, `EffectCtx`, the descriptor types, …) plus
/// this crate's typed convenience layer. The moved primitive sources reach both
/// through `crate::rt::…`, so they need no per-symbol classification of which
/// half a name came from.
pub(crate) mod rt {
    pub use crate::typed_primitive::*;
    pub use vix::runtime::*;
}

/// The host types `vixen` declares — the domain types `Tree` and `TreeEntry`,
/// injected into the compiler through [`vix::compiler::CompilerConfig::host_types`]
/// so the bare language no longer hardcodes them as `ExternKind` variants (issue
/// 2520). They remain extern-backed opaque values with byte-stable `vix.Tree` /
/// `vix.TreeEntry` identity; only the *declaration* moves here. `TreeEntry` is now
/// vestigial — the standalone `receiver / segment` projection and `TreeEntry.text()`
/// method were retired once exec-origin reads moved onto the `tree_read` primitive
/// — but it stays declared to keep its nominal identity stable.
pub const HOST_TYPES: &[vix::binding::HostTypeDecl] = &[
    vix::binding::HostTypeDecl {
        name: vix::binding::TREE,
    },
    vix::binding::HostTypeDecl {
        name: vix::binding::TREE_ENTRY,
    },
];

/// The capability types a program may receive as `#[test]` parameters,
/// injected into the compiler through
/// [`vix::compiler::CompilerConfig::capabilities`]. They live here for the same
/// reason `untar` does: a tool name is a capability package's, never the
/// machine's, and `machine.capability.no-argv-dialect` bans a per-tool arm in
/// machine code. `vix-core` now spells none of these names.
///
/// This is *derived*, not declared: the nameable types are exactly the
/// registered packages ([`capability_package::registered_package_names`]), so
/// registering a package is the single act that makes a tool nameable. The two
/// used to be separate hand-maintained lists that a test held in agreement,
/// which meant a new tool needed two Rust edits to be usable and one to be
/// silently unusable.
///
/// The shipped packages are registered here rather than assumed: this function
/// READS the registry, so a caller that had not registered them yet would be
/// handed an empty nameable set — a compiler in which `Sh` does not resolve,
/// reported as the program naming an unknown type. Registration is idempotent,
/// so the entrypoints that also register lose nothing by it.
///
/// The returned slice is leaked because
/// [`vix::compiler::CompilerConfig::capabilities`] is `&'static`, and memoized
/// on the name set so the leak really is bounded by the number of distinct
/// registered sets a process builds a config from rather than by how many
/// configs it builds.
///
/// r[impl machine.primitive.capabilities-by-identity]
#[must_use]
pub fn capability_types() -> &'static [vix::binding::CapabilityTypeDecl] {
    // An embedder that registered a contradicting `Sh` before this point is a
    // conflict the entrypoint reports as a typed failure
    // (`vixen_runtime::manifest::declared_packages`). Dropping it here leaves
    // that name bound to the embedder's grammar and the rest of the shipped
    // set registered as usual — it can never bind a name to the wrong package.
    let _ = capability_package::register_default_packages();
    let names = capability_package::registered_package_names();
    let mut memo = CAPABILITY_TYPES.lock().expect("type memo is not poisoned");
    if let Some((_, decls)) = memo.iter().find(|(registered, _)| *registered == names) {
        return decls;
    }
    let decls: &'static [vix::binding::CapabilityTypeDecl] = Box::leak(
        names
            .iter()
            .map(|&name| vix::binding::CapabilityTypeDecl { name })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    );
    memo.push((names, decls));
    decls
}

/// The leaked slices [`capability_types`] has already handed out, keyed by the
/// registered name set they were built from.
static CAPABILITY_TYPES: std::sync::Mutex<
    Vec<(
        Vec<&'static str>,
        &'static [vix::binding::CapabilityTypeDecl],
    )>,
> = std::sync::Mutex::new(Vec::new());

/// Reserve `vixen`'s host-extern type names ([`HOST_TYPES`]) with `vix-core`'s
/// schema batch — the identity anchor that lets an injected `HostTypeDecl`
/// resolve rather than be rejected as unregistered. Must run before the first
/// compilation that declares these host types (the runtime does this at
/// assembly; tests that inject [`HOST_TYPES`] directly call it themselves).
/// Additive and idempotent, so calling it more than once is harmless.
pub fn register_host_types() {
    let names: Vec<&'static str> = HOST_TYPES.iter().map(|decl| decl.name).collect();
    vix::schema::register_host_externs(&names);
}

/// The host-type methods `vixen` declares on the domain types, injected into the
/// compiler through [`vix::compiler::CompilerConfig::methods`]. `Tree.glob` and
/// `Blob.len` name dedicated ops whose bespoke lowering and execution stay in
/// `vix-core` (the machine engine) — only their *declarations* live here.
/// `Registry.url` rides the fully open rail: a primitive-backed method whose
/// whole contract (request record, result type, primitive id) is declared in
/// this crate beside its implementation, with no dedicated op in core at all
/// (issue 2520). (`.text()` is no longer a dedicated method: exec-origin tree
/// reads spell the `tree_read` primitive request directly.)
pub const DOMAIN_METHODS: &[vix::binding::MethodDecl] = &[
    vix::binding::MethodDecl {
        receiver: vix::binding::ReceiverType::Host(vix::binding::TREE),
        name: "glob",
        arity: 1,
        lowering: vix::binding::MethodLowering::CodataPrimitive(vix::binding::CodataMethodDecl {
            operands: &[tree_glob_pattern_type],
            result: tree_glob_stream_type,
            id: crate::rt::tree_glob_primitive_id,
        }),
    },
    vix::binding::MethodDecl {
        receiver: vix::binding::ReceiverType::Blob,
        name: "gunzip",
        arity: 0,
        lowering: vix::binding::MethodLowering::Primitive(vix::binding::PrimitiveMethodDecl {
            request: blob_gunzip_request_type,
            result: blob_gunzip_result_type,
            id: blob_gunzip_primitive_id,
        }),
    },
    vix::binding::MethodDecl {
        receiver: vix::binding::ReceiverType::Blob,
        name: "try_text",
        arity: 0,
        lowering: vix::binding::MethodLowering::Primitive(vix::binding::PrimitiveMethodDecl {
            request: blob_text_request_type,
            result: blob_text_result_type,
            id: blob_text_primitive_id,
        }),
    },
    vix::binding::MethodDecl {
        receiver: vix::binding::ReceiverType::Blob,
        name: "len",
        arity: 0,
        lowering: vix::binding::MethodLowering::Primitive(vix::binding::PrimitiveMethodDecl {
            request: blob_len_request_type,
            result: blob_len_result_type,
            id: blob_len_primitive_id,
        }),
    },
    vix::binding::MethodDecl {
        receiver: vix::binding::ReceiverType::Registry,
        name: "url",
        arity: 1,
        lowering: vix::binding::MethodLowering::Primitive(vix::binding::PrimitiveMethodDecl {
            request: registry_url_request_type,
            result: registry_url_result_type,
            id: registry_url_primitive_id,
        }),
    },
];

/// The `String` glob pattern operand of `Tree.glob` — the sole operand of its
/// [`vix::binding::CodataMethodDecl`]. A `fn` so the declaration stays `const`.
fn tree_glob_pattern_type() -> vix::vir::Type {
    vix::vir::Type::String
}

/// The `Stream<Path, Path>` recipe `Tree.glob` produces.
fn tree_glob_stream_type() -> vix::vir::Type {
    vix::vir::Type::stream(vix::vir::Type::Path, vix::vir::Type::Path)
}

/// The free-function primitive surfaces `vixen` injects into a compilation —
/// the contracts behind prelude names that `vix-core` does not know.
///
/// `vix-core`'s own [`vix::runtime::builtin_primitive_surfaces`] carries the
/// language's bundled surfaces (`fetch`); this is the embedder half of the same
/// rail, consumed through `Compiler::with_primitive_surfaces`. An injected
/// surface answers to both its bare prelude name (`untar`) and its qualified
/// spelling (`std::untar`), exactly as a bundled one does.
///
/// `untar` is here rather than in core because it stopped being a machine op:
/// it is an ordinary hermetic primitive in this crate now (issue 2597), and the
/// bare language no longer spells its name anywhere.
#[must_use]
pub fn injected_primitive_surfaces() -> Vec<vix::runtime::PrimitiveSurface> {
    vec![vix::runtime::PrimitiveSurface {
        surface_name: UNTAR_DECL.name,
        id: untar_primitive_id(),
        shape: vix::runtime::synth_shape(&UNTAR_DECL, untar_request_type(), untar_result_type()),
    }]
}

pub use blob_gunzip_primitive::*;
pub use blob_len_primitive::*;
pub use blob_slice_primitive::*;
pub use blob_text_primitive::*;
pub use decode_primitive::*;
pub use exec_primitive::*;
pub use fetch_primitive::*;
pub use http_origin::*;
pub use primitive_value_decode::*;
pub use registry_url_primitive::*;
pub use tree_glob_primitive::*;
pub use tree_read_primitive::*;
pub use typed_primitive::*;
pub use untar_primitive::*;
