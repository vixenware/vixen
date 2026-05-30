//! The runtime value model — the spine of the demand-driven runtime.
//!
//! The single most important property, and the one every prior implementation
//! got wrong (`EvalValue::Pending(String)`):
//!
//! > **Pending is a property of *any* value of *any* type, carrying a *resolver*
//! > that says how it resolves — never a string, never a not-implemented
//! > sentinel.** (`r[eval.pending]`; `docs/design/runtime-semantics.md` §3.)
//!
//! Two structural incentives are encoded directly in these types, so the
//! eager-tree-walk shortcut cannot be written:
//!
//! 1. **No bare scalar escapes the orchestrator.** A [`Value`] is `Resolved |
//!    Pending`; there is deliberately *no* public `fn as_u64(&self) -> u64`. To
//!    learn a value's content you must force it through the evaluator, and inside
//!    the orchestrator nothing forces (`r[eval.forcing]`). So `expect_u64(&v)`
//!    has nothing to return — the eager interpreter does not typecheck.
//! 2. **Pending carries a [`Resolver`], not a `String`.** "Not yet supported" is a
//!    compile error or an explicit `todo!()`, never a value that quietly flows and
//!    poisons. Nothing here is a string.
//!
//! Pendingness is **per position**: compound payloads ([`Payload::Pair`],
//! [`Payload::Struct`], [`Payload::Array`]) hold [`Value`]s, so one field can be
//! pending while its siblings are resolved — "a struct can have some fields
//! resolved and some pending" (`r[eval.pending]`).

use std::sync::Arc;

/// A runtime value: resolved to a concrete payload, or pending with a resolver
/// describing how it becomes resolved.
///
/// This is the *only* currency inside the orchestrator. You never hold a bare
/// [`Payload`] there — that is the whole point (see the module docs).
#[derive(Debug, Clone)]
pub enum Value {
    /// A fully resolved payload. (Itself may *contain* pending sub-values.)
    Resolved(Payload),
    /// Not yet resolved; [`Pending::resolver`] says how it will be.
    Pending(Pending),
}

/// The resolved content of a value.
///
/// Kernel-visible types have a canonical payload layout (`kernel-abi.md`); the
/// runtime carries this envelope-rich form, the JIT the word-flattened ABI form.
/// Compound payloads hold [`Value`]s (not raw `Payload`s) so that pendingness
/// nests to any depth.
#[derive(Debug, Clone)]
pub enum Payload {
    U64(u64),
    Bool(bool),
    /// Reference-counted UTF-8 (`i[impl.string]`).
    Str(Str),
    /// Positional product. Fields are addressed by index (`pair[0]`, never `._0`;
    /// see `query-surface`). Each element is a [`Value`] — either may be pending.
    Pair(Arc<Value>, Arc<Value>),
    /// Named product (`i[impl.struct]`): fixed fields, each a reference-counted
    /// [`Value`], so updating one field shares the rest (`with`).
    Struct(Struct),
    /// Persistent array (`i[impl.array]`): O(1) clone, O(log n) update.
    Array(Array),
    /// A function pointer (a kernel/closure body) plus captured [`Value`]s
    /// (`i[impl.closure]`).
    Closure(Closure),
    /// A content-addressed tree handle. Note its **identity** may itself be pending
    /// (an `exec` output whose hash is only discovered post-run) — see
    /// [`TreeHandle`]. Identity (the hash) and materialization (where the bytes
    /// live) are independent axes (`runtime-semantics.md` §4).
    Tree(TreeHandle),
}

/// A pending value: the type it *will* have once resolved, plus the [`Resolver`]
/// that produces it. **There is no `String` here, by design.**
#[derive(Debug, Clone)]
pub struct Pending {
    /// The statically known type this resolves to. (Pending values are typed even
    /// before they have content — that is what makes a pending `U64` distinct from
    /// a pending `Tree`.)
    pub ty: TypeRef,
    pub resolver: Resolver,
}

/// How a pending value becomes resolved. Forcing dispatches on this.
///
/// This is the heart of the demand model: a pending value is not a placeholder
/// with a label, it is a *suspended computation with a known shape*.
#[derive(Debug, Clone)]
pub enum Resolver {
    /// A pure **kernel** — an eager island. Forcing it forces the kernel's inputs
    /// (demand-guided: only the inputs actually reached), runs the kernel to
    /// completion, and lifts the result. This is the *legitimate* eager part of
    /// the engine; eagerness is correct *inside* a kernel, wrong across the sea.
    Kernel {
        kernel: KernelId,
        /// Inputs the kernel consumes. Each is a [`Value`] — forcing the kernel
        /// forces only those its control flow demands.
        inputs: Arc<[Value]>,
    },

    /// A **boundary** node (`exec` / `acquire` / `Http` / …). Forcing it serializes
    /// the upfront inputs (`r[eval.forcing.boundary]`), dispatches to an executor,
    /// and — for the requester — pulls the produced parts on demand. See the `exec`
    /// demand profile (`runtime-semantics.md` §5): upfront → L1 key, progressive →
    /// read-set, produced → forced by the requester.
    Boundary { boundary: BoundaryId },

    /// A **stream** — resolves incrementally, chunk by chunk, over time. Consumed
    /// as it arrives; collected to an [`Payload::Array`] only at a forcing boundary
    /// (the `Stream -> Array` coercion, `query-surface`). Distinct from ordinary
    /// pending, which resolves *once*; a stream resolves *many times*.
    Stream { stream: StreamId },

    /// `💣` — the demand tripwire. **Forcing it poisons.** It is dropped into a
    /// position a correct demand-driven evaluation must *never reach*; if it stays
    /// unforced (demand never arrives), it is inert and the program is fine. An
    /// engine that over-forces detonates it and the test goes **red**.
    ///
    /// This is NOT an inert sentinel (the `Pending(String)` mistake): its whole
    /// purpose is to detonate *on force*. It is a `Resolver` precisely so that a
    /// bomb is just "a pending value whose resolution poisons."
    Bomb,
}

// ---------------------------------------------------------------------------
// Compound payload bodies
// ---------------------------------------------------------------------------

/// Reference-counted UTF-8 bytes (`i[impl.string]`). Small-string optimization and
/// interning come later; the contract is value semantics + O(1) clone.
#[derive(Debug, Clone)]
pub struct Str(pub Arc<str>);

/// A named product. Fields are in declaration order; each value may be pending.
#[derive(Debug, Clone)]
pub struct Struct {
    /// `(field name, value)` in declaration order. Reference-counted so a `with`
    /// update shares the untouched fields.
    pub fields: Arc<[(Symbol, Value)]>,
}

/// A persistent array (`i[impl.array]`). Backed by an `Arc<[Value]>` for Phase 0;
/// this becomes an RRB tree when O(log n) update matters.
#[derive(Debug, Clone)]
pub struct Array {
    pub items: Arc<[Value]>,
}

/// A closure: a kernel body plus its resolved-or-pending captures
/// (`i[impl.closure]`). The unit shipped to an executor for an `exec` observer is
/// the same shape (`kernel-abi.md`: one mechanism, not two).
#[derive(Debug, Clone)]
pub struct Closure {
    pub body: KernelId,
    pub captures: Arc<[Value]>,
}

/// A content-addressed tree handle (`i[impl.tree]`). Identity and materialization
/// are independent (`runtime-semantics.md` §4):
///
/// - [`TreeHandle::Known`] — identity resolved (we have the hash); the bytes may
///   live in the local fs, the CAS, or a remote executor (materialization is a
///   separate concern, tracked elsewhere).
/// - [`TreeHandle::IdentityPending`] — identity *not yet known*: produced by a
///   boundary (an `exec` write-set), the hash is discovered when it resolves.
///   This is precisely why an `exec` output "can't be analyzed statically."
#[derive(Debug, Clone)]
pub enum TreeHandle {
    Known(ContentHash),
    IdentityPending(BoundaryId),
}

// ---------------------------------------------------------------------------
// Identifiers (Phase 0 placeholders — bound to VIR / the store at wiring time)
// ---------------------------------------------------------------------------

/// An interned symbol (field/binding name). Phase 0 placeholder; becomes an
/// interned id when the front-end is wired in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Symbol(pub Arc<str>);

/// Index of a kernel in a demand graph's kernel table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KernelId(pub u32);

/// Index of a boundary node in a demand graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BoundaryId(pub u32);

/// Index of a stream in a demand graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StreamId(pub u32);

/// A reference to a (VIR) type. **Phase 0 placeholder** — keeps `vx-runtime`
/// self-contained; binds to the real `vx-ir` type id when the front-end is wired
/// in. The point of carrying it now is that pending values are *typed*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeRef(pub u32);

/// A content hash (Blake3). **Phase 0 placeholder** — unify with
/// `vx-services::Blake3Hash` at wiring time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContentHash(pub [u8; 32]);

// ---------------------------------------------------------------------------
// Constructors / a few non-forcing observations
// ---------------------------------------------------------------------------

impl Value {
    /// A resolved `U64`.
    pub fn u64(v: u64) -> Self {
        Value::Resolved(Payload::U64(v))
    }

    /// A resolved `Bool`.
    pub fn bool(v: bool) -> Self {
        Value::Resolved(Payload::Bool(v))
    }

    /// A bomb: a pending value of the given (bottom) type whose forcing poisons.
    /// Dropped where demand must never reach (`runtime-semantics.md` §0).
    pub fn bomb(ty: TypeRef) -> Self {
        Value::Pending(Pending {
            ty,
            resolver: Resolver::Bomb,
        })
    }

    /// Whether this value is resolved *at the top level*. Deliberately the only
    /// thing you can ask without forcing — it does **not** let you read the
    /// payload. (Forcing lives in the evaluator, not on `Value`.)
    pub fn is_resolved(&self) -> bool {
        matches!(self, Value::Resolved(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_carries_a_typed_resolver_not_a_string() {
        // The shape we are locking in: a pending value is a typed, suspended
        // computation — never a label. This test exists to make that structural.
        let v = Value::bomb(TypeRef(0));
        match v {
            Value::Pending(Pending {
                resolver: Resolver::Bomb,
                ..
            }) => {}
            _ => panic!("bomb must be a pending value with a Bomb resolver"),
        }
    }

    #[test]
    fn a_struct_can_be_partly_pending() {
        // r[eval.pending]: some fields resolved, some pending — per position.
        let s = Struct {
            fields: Arc::from(
                [
                    (Symbol(Arc::from("a")), Value::u64(42)),
                    (Symbol(Arc::from("b")), Value::bomb(TypeRef(0))),
                ]
                .to_vec(),
            ),
        };
        assert!(s.fields[0].1.is_resolved());
        assert!(!s.fields[1].1.is_resolved());
    }
}
