//! Kernel IR + the subroutine-threaded interpreter — the JIT oracle / stand-in
//! (rebuild-plan Phase 2/6, mirroring `phon-jit`).
//!
//! A **kernel** is an eager, pure, straight-line region (with kernel-internal
//! control flow) extracted from the demand graph's `Kernel`-classified regions.
//! It is the unit the copy-and-patch JIT will compile. Per `kernel-abi.md` it
//! operates on a flat array of **ABI words** ([`Word`] = `u64`):
//!
//! - **scalars are unboxed** — a `U64` slot *is* the integer, `Bool` is `0`/`1`.
//!   `op_add` is literally `slots[dst] = slots[a] + slots[b]`; no box, no lookup.
//! - wider values (string `[ptr,len]`, flattened structs) occupy more slots — the
//!   layout-descriptor work, deferred. This first cut is scalar-only (width 1).
//!
//! Each op is a **stencil**: a small `fn(&mut [Word], &Op) -> Result<(), Poison>`.
//! The interpreter here threads them through function pointers (phon's stand-in);
//! the future native backend will `memcpy` the same functions' bytes and patch the
//! immediates — **same functions, same word ABI, identical results** (the only
//! difference is the stitching, so this interpreter is the JIT's oracle). Control
//! flow is **basic blocks + a conditional branch** (short-circuit: only the taken
//! block runs), which the native backend chains by **relocation** (phon's
//! `patch_branch26`), never by evaluating both sides.

use crate::eval::Value;
use crate::graph::Span;

/// One ABI word. Scalars are unboxed into it; wider values are carried by
/// **handle** — a word that indexes the kernel's [`value_pool`](Kernel::value_pool)
/// side table (the width-1 alternative to flattening a string/struct across slots).
pub type Word = u64;

/// An SSA register index into the kernel's slot array (params first).
pub type Slot = u32;

/// A poison — a detonation reason plus **where in the source** it detonated
/// (I6 — source fidelity). Returned across a stencil boundary, never panicked
/// (phon's rule). The span rides the IR, so a poison three layers down and one
/// host over still points at the original line.
#[derive(Debug, Clone)]
pub struct Poison {
    /// The typed domain error (the failure model's `err`). A `fail e` in the
    /// kernel carries `e`'s value via the [`value_pool`](Kernel::value_pool);
    /// internal faults (divide-by-zero) carry a `Value::Str`.
    pub err: Value,
    pub span: Option<Span>,
}

/// The fixed stencil-op vocabulary. One variant ⇒ one stencil function ⇒ one
/// machine-code fragment. Scalar-first; string/struct ops arrive with layout
/// descriptors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    /// `dst = imm`
    Const,
    /// `dst = slots[a]` — a slot move. The de-SSA'd φ: both sides of an `if`
    /// write their value into one shared result slot via `Copy`, so the join
    /// block reads a single slot regardless of which branch ran.
    Copy,
    // arithmetic (U64, wrapping)
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    // comparison → Bool (0/1)
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    // logic on 0/1
    And,
    Or,
    Not,
    /// poison unconditionally (`fail`)
    Fail,
}

/// One operation: a stencil kind, its destination slot, up to two operand slots,
/// and an immediate (`Const`). Unused fields are `0`. (This is the
/// `(stencil, immediates)` entry the copy-and-patch backend will splice.)
#[derive(Debug, Clone)]
pub struct Op {
    pub kind: OpKind,
    pub dst: Slot,
    pub a: Slot,
    pub b: Slot,
    pub imm: Word,
    /// Source span this op lowered from — carried so a poison points at source
    /// (I6). `None` for synthetic ops (e.g. a φ `Copy`) and hand-built kernels.
    pub span: Option<Span>,
}

/// A basic block: straight-line ops then a terminator.
#[derive(Debug, Clone)]
pub struct Block {
    pub ops: Vec<Op>,
    pub term: Term,
}

/// How a block ends. Branches are the *only* way control flow happens — there is
/// no eager `select`, so a dead branch (and any bomb that lowered into it) is
/// never run.
#[derive(Debug, Clone, Copy)]
pub enum Term {
    /// Kernel result = `slots[_]`.
    Return(Slot),
    /// Unconditional jump to a block.
    Jump(u32),
    /// `slots[cond] != 0` → `then_blk`, else `else_blk`.
    BranchIf {
        cond: Slot,
        then_blk: u32,
        else_blk: u32,
    },
}

/// A kernel: a small pure function over word slots. The first `n_params` slots
/// are the inputs; block `0` is the entry.
#[derive(Debug, Clone)]
pub struct Kernel {
    pub n_slots: u32,
    pub n_params: u32,
    pub blocks: Vec<Block>,
    /// The source span of the region this kernel lowered from — the coarse
    /// fallback a poison reports when the precise op isn't attributable (the
    /// native JIT path, until per-op fault attribution lands).
    pub span: Option<Span>,
    /// The **side table** of non-scalar values a `Fail` op can carry as its `err`
    /// (the failure model). A `Fail` op's `imm` indexes here; an out-of-range `imm`
    /// (e.g. a bare `fail`) means no payload → a generic `"fail"`.
    pub value_pool: Vec<Value>,
}

/// The `err` value a reached `Fail` op detonates with — its pooled payload, or a
/// generic `"fail"` when it carries none (bare `fail`, or a hand-built kernel).
fn fail_err(k: &Kernel, op: &Op) -> Value {
    k.value_pool
        .get(op.imm as usize)
        .cloned()
        .unwrap_or_else(|| Value::Str(std::rc::Rc::from("fail")))
}

/// The typed `err` the faulting `op` detonates with — the single source of truth
/// for both the interpreter run loop and the JIT driver, so the two modes report
/// identical errors (the cross-mode-parity oracle depends on it).
pub(crate) fn op_err(k: &Kernel, op: &Op) -> Value {
    match op.kind {
        OpKind::Fail => fail_err(k, op),
        OpKind::Div => str_err("divide by zero"),
        OpKind::Rem => str_err("remainder by zero"),
        _ => str_err("kernel poisoned"),
    }
}

/// A stencil: the per-op fragment. The interpreter calls it through this pointer;
/// the JIT will copy its bytes and patch the immediates.
type Stencil = fn(&mut [Word], &Op) -> Result<(), Poison>;

// --- the stencils (one per op; scalars unboxed, raw word arithmetic) ---

fn st_const(s: &mut [Word], op: &Op) -> Result<(), Poison> {
    s[op.dst as usize] = op.imm;
    Ok(())
}
fn st_copy(s: &mut [Word], op: &Op) -> Result<(), Poison> {
    s[op.dst as usize] = s[op.a as usize];
    Ok(())
}
macro_rules! arith {
    ($name:ident, $f:expr) => {
        fn $name(s: &mut [Word], op: &Op) -> Result<(), Poison> {
            let (a, b) = (s[op.a as usize], s[op.b as usize]);
            s[op.dst as usize] = $f(a, b);
            Ok(())
        }
    };
}
arith!(st_add, |a: Word, b: Word| a.wrapping_add(b));
arith!(st_sub, |a: Word, b: Word| a.wrapping_sub(b));
arith!(st_mul, |a: Word, b: Word| a.wrapping_mul(b));
arith!(st_eq, |a, b| (a == b) as Word);
arith!(st_ne, |a, b| (a != b) as Word);
arith!(st_lt, |a, b| (a < b) as Word);
arith!(st_le, |a, b| (a <= b) as Word);
arith!(st_gt, |a, b| (a > b) as Word);
arith!(st_ge, |a, b| (a >= b) as Word);
arith!(st_and, |a, b| ((a != 0) && (b != 0)) as Word);
arith!(st_or, |a, b| ((a != 0) || (b != 0)) as Word);

fn st_not(s: &mut [Word], op: &Op) -> Result<(), Poison> {
    s[op.dst as usize] = (s[op.a as usize] == 0) as Word;
    Ok(())
}
fn st_div(s: &mut [Word], op: &Op) -> Result<(), Poison> {
    let b = s[op.b as usize];
    if b == 0 {
        return Err(Poison {
            err: str_err("divide by zero"),
            span: op.span,
        });
    }
    s[op.dst as usize] = s[op.a as usize] / b;
    Ok(())
}
fn st_rem(s: &mut [Word], op: &Op) -> Result<(), Poison> {
    let b = s[op.b as usize];
    if b == 0 {
        return Err(Poison {
            err: str_err("remainder by zero"),
            span: op.span,
        });
    }
    s[op.dst as usize] = s[op.a as usize] % b;
    Ok(())
}
/// Bare-fallback stencil — the run loop resolves a `Fail`'s pooled `err` (it has
/// the `Kernel`); this is only reached for a hand-threaded call without one.
fn st_fail(_: &mut [Word], op: &Op) -> Result<(), Poison> {
    Err(Poison {
        err: str_err("fail"),
        span: op.span,
    })
}

/// An internal kernel fault (not a user `fail`) → a `Value::Str` err.
fn str_err(msg: &str) -> Value {
    Value::Str(std::rc::Rc::from(msg))
}

/// The stencil for an op kind — the function-pointer table the interpreter
/// threads and the JIT will extract bytes from.
fn stencil_for(kind: OpKind) -> Stencil {
    use OpKind::*;
    match kind {
        Const => st_const,
        Copy => st_copy,
        Add => st_add,
        Sub => st_sub,
        Mul => st_mul,
        Div => st_div,
        Rem => st_rem,
        Eq => st_eq,
        Ne => st_ne,
        Lt => st_lt,
        Le => st_le,
        Gt => st_gt,
        Ge => st_ge,
        And => st_and,
        Or => st_or,
        Not => st_not,
        Fail => st_fail,
    }
}

/// Run a kernel over `args` (bound to the first `n_params` slots), threading the
/// stencils. The result is the value of the slot named by the terminating
/// `Return`. This is the **oracle**: the JIT must produce identical results.
pub fn run(k: &Kernel, args: &[Word]) -> Result<Word, Poison> {
    let mut slots = vec![0 as Word; k.n_slots as usize];
    for (i, &a) in args.iter().take(k.n_params as usize).enumerate() {
        slots[i] = a;
    }
    let mut blk = 0usize;
    // A generous step budget guards against a malformed (cyclic) block graph.
    for _ in 0..1_000_000 {
        let block = &k.blocks[blk];
        for op in &block.ops {
            // `Fail` resolves its `err` from the value pool (needs `k`); every other
            // op is a pure slot transform via its stencil.
            if op.kind == OpKind::Fail {
                return Err(Poison {
                    err: fail_err(k, op),
                    span: op.span,
                });
            }
            stencil_for(op.kind)(&mut slots, op)?;
        }
        match &block.term {
            Term::Return(s) => return Ok(slots[*s as usize]),
            Term::Jump(t) => blk = *t as usize,
            Term::BranchIf {
                cond,
                then_blk,
                else_blk,
            } => {
                blk = if slots[*cond as usize] != 0 {
                    *then_blk
                } else {
                    *else_blk
                } as usize;
            }
        }
    }
    Err(Poison {
        err: str_err("kernel step budget exhausted (cyclic blocks?)"),
        span: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(kind: OpKind, dst: Slot, a: Slot, b: Slot) -> Op {
        Op {
            kind,
            dst,
            a,
            b,
            imm: 0,
            span: None,
        }
    }
    fn konst(dst: Slot, imm: Word) -> Op {
        Op {
            kind: OpKind::Const,
            dst,
            a: 0,
            b: 0,
            imm,
            span: None,
        }
    }

    #[test]
    fn arithmetic_kernel() {
        // (1 + 2) * 3  ->  9.  slots: 0=1, 1=2, 2=sum, 3=3, 4=prod.
        let k = Kernel {
            n_slots: 5,
            n_params: 0,
            blocks: vec![Block {
                ops: vec![
                    konst(0, 1),
                    konst(1, 2),
                    op(OpKind::Add, 2, 0, 1),
                    konst(3, 3),
                    op(OpKind::Mul, 4, 2, 3),
                ],
                term: Term::Return(4),
            }],
            span: None,
            value_pool: vec![],
        };
        assert_eq!(run(&k, &[]).unwrap(), 9);
    }

    /// `assert_eq`'s body: `if got == want ( true ) else ( fail )`.
    /// params: 0=got, 1=want. slots: 2=cond, 3=true.
    fn assert_eq_kernel() -> Kernel {
        Kernel {
            n_slots: 4,
            n_params: 2,
            blocks: vec![
                // entry: cond = got == want; branch
                Block {
                    ops: vec![op(OpKind::Eq, 2, 0, 1)],
                    term: Term::BranchIf {
                        cond: 2,
                        then_blk: 1,
                        else_blk: 2,
                    },
                },
                // then: return true
                Block {
                    ops: vec![konst(3, 1)],
                    term: Term::Return(3),
                },
                // else: fail (the dead branch — only reached when not equal)
                Block {
                    ops: vec![op(OpKind::Fail, 0, 0, 0)],
                    term: Term::Return(3),
                },
            ],
            span: None,
            value_pool: vec![],
        }
    }

    #[test]
    fn branch_takes_only_the_live_side() {
        let k = assert_eq_kernel();
        // equal -> then -> true (1); the `fail` in the dead branch never runs.
        assert_eq!(run(&k, &[42, 42]).unwrap(), 1);
        // not equal -> else -> fail poisons.
        assert!(run(&k, &[42, 43]).is_err());
    }

    #[test]
    fn divide_by_zero_poisons_as_a_returned_value() {
        let k = Kernel {
            n_slots: 3,
            n_params: 2,
            blocks: vec![Block {
                ops: vec![op(OpKind::Div, 2, 0, 1)],
                term: Term::Return(2),
            }],
            span: None,
            value_pool: vec![],
        };
        assert_eq!(run(&k, &[10, 2]).unwrap(), 5);
        assert!(run(&k, &[10, 0]).is_err()); // no panic across the stencil boundary
    }
}
