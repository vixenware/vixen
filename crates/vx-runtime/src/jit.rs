//! The native copy-and-patch backend (rebuild-plan step 5), on Apple Silicon.
//!
//! `kernel.rs` is the **oracle**: a subroutine-threaded interpreter over the same
//! stencils. This module runs the *same* operations as machine code — rustc emits
//! each stencil at build time (`stencils/stencils.rs` → `build.rs`), and here we
//! `memcpy` the stencil bytes into one buffer, patch each op's continuation branch
//! to the next (`copypatch::patch_branch26`), and execute it from `MAP_JIT` memory
//! (`copypatch::ExecBuf`). Same operations, same word ABI, identical results — the
//! differential test asserts `jit_run == kernel::run` on every kernel it accepts.
//!
//! **Scope: straight-line kernels** (one basic block, a `Return` terminator).
//! That is the whole scalar-arithmetic class — `Const`, `Copy`, the binary ops,
//! `Not`, and the poisons (`Fail`, divide/remainder by zero, via a status flag and
//! an early `ret`, no panic across the boundary). Control flow (`if` → multiple
//! blocks, `BranchIf`/`Jump`) needs a conditional-branch relocation `copypatch`
//! doesn't expose yet; [`jit_run`] returns `None` for those and the caller runs the
//! interpreter. Phon's substrate grows the same way (smoke → scalar → …).

use copypatch::{ExecBuf, patch_branch26};

use crate::kernel::{Kernel, Op, OpKind, Poison, Term, Word};
use crate::stencils;

/// Threaded state, matching `Ctx` in `stencils/stencils.rs` byte for byte.
#[repr(C)]
struct Ctx {
    slots: *mut u64,
    prog: *const u64,
    status: u64,
    ret: u64,
}

/// Run `smoke(x) = x*3 + 1` as rustc-emitted machine code from a `MAP_JIT` page —
/// the canary that the native path works on this host. Returns `None` off the
/// native target (empty stencil table).
#[must_use]
pub fn smoke(x: u64) -> Option<u64> {
    if stencils::SMOKE.is_empty() {
        return None;
    }
    let buf = ExecBuf::new(stencils::SMOKE);
    let f: extern "C" fn(u64) -> u64 = unsafe { core::mem::transmute(buf.as_ptr()) };
    Some(f(x))
}

/// JIT-compile and run a **straight-line** kernel over `args` (bound to slots
/// `0..n_params`). Returns `None` if the kernel has control flow (the caller falls
/// back to [`kernel::run`](crate::kernel::run)) or the native backend is absent.
#[must_use]
pub fn jit_run(k: &Kernel, args: &[Word]) -> Option<Result<Word, Poison>> {
    // Straight-line only: one block ending in `Return`.
    if k.blocks.len() != 1 {
        return None;
    }
    let block = &k.blocks[0];
    let Term::Return(ret_slot) = block.term else {
        return None;
    };
    // Off the native target the table is empty — no JIT.
    if stencils::RETURN.is_empty() {
        return None;
    }

    // Lay out the stencil chain: a copy per op (immediates threaded into `prog`),
    // then the `return` terminator.
    let mut code: Vec<u8> = Vec::new();
    let mut prog: Vec<u64> = Vec::new();
    let mut starts: Vec<usize> = Vec::with_capacity(block.ops.len());
    for op in &block.ops {
        starts.push(code.len());
        code.extend_from_slice(stencil_bytes(op.kind));
        push_imms(&mut prog, op);
    }
    let ret_start = code.len();
    code.extend_from_slice(stencils::RETURN);
    prog.push(ret_slot as u64);

    // Patch each chaining op's continuation to the next op (last op → `return`).
    for (i, &start) in starts.iter().enumerate() {
        let next = starts.get(i + 1).copied().unwrap_or(ret_start);
        for &rel in stencil_cont(block.ops[i].kind) {
            patch_branch26(&mut code, start + rel, next);
        }
    }

    // Bind args, run from executable memory, read back the result / poison.
    let mut slots = vec![0u64; k.n_slots as usize];
    for (i, &a) in args.iter().take(k.n_params as usize).enumerate() {
        slots[i] = a;
    }
    let buf = ExecBuf::new(&code);
    let mut ctx = Ctx {
        slots: slots.as_mut_ptr(),
        prog: prog.as_ptr(),
        status: 0,
        ret: 0,
    };
    let entry: extern "C" fn(*mut Ctx) = unsafe { core::mem::transmute(buf.as_ptr()) };
    entry(&mut ctx);

    Some(if ctx.status != 0 {
        Err(Poison("kernel poisoned".into()))
    } else {
        Ok(ctx.ret)
    })
}

/// The machine code for an op's stencil.
fn stencil_bytes(kind: OpKind) -> &'static [u8] {
    use OpKind::*;
    match kind {
        Const => stencils::CONST,
        Copy => stencils::COPY,
        Add => stencils::ADD,
        Sub => stencils::SUB,
        Mul => stencils::MUL,
        Div => stencils::DIV,
        Rem => stencils::REM,
        Eq => stencils::EQ,
        Ne => stencils::NE,
        Lt => stencils::LT,
        Le => stencils::LE,
        Gt => stencils::GT,
        Ge => stencils::GE,
        And => stencils::AND,
        Or => stencils::OR,
        Not => stencils::NOT,
        Fail => stencils::FAIL,
    }
}

/// The continuation-relocation offsets for an op's stencil (empty for `Fail`,
/// which just `ret`s — it never chains).
fn stencil_cont(kind: OpKind) -> &'static [usize] {
    use OpKind::*;
    match kind {
        Const => stencils::CONST_CONT,
        Copy => stencils::COPY_CONT,
        Add => stencils::ADD_CONT,
        Sub => stencils::SUB_CONT,
        Mul => stencils::MUL_CONT,
        Div => stencils::DIV_CONT,
        Rem => stencils::REM_CONT,
        Eq => stencils::EQ_CONT,
        Ne => stencils::NE_CONT,
        Lt => stencils::LT_CONT,
        Le => stencils::LE_CONT,
        Gt => stencils::GT_CONT,
        Ge => stencils::GE_CONT,
        And => stencils::AND_CONT,
        Or => stencils::OR_CONT,
        Not => stencils::NOT_CONT,
        Fail => &[],
    }
}

/// Push an op's immediates onto the `prog` stream in the order its stencil reads
/// them: `Const` → `[dst, imm]`, `Copy`/`Not` → `[dst, a]`, `Fail` → none, every
/// other op → `[dst, a, b]`.
fn push_imms(prog: &mut Vec<u64>, op: &Op) {
    use OpKind::*;
    match op.kind {
        Const => {
            prog.push(op.dst as u64);
            prog.push(op.imm);
        }
        Copy | Not => {
            prog.push(op.dst as u64);
            prog.push(op.a as u64);
        }
        Fail => {}
        _ => {
            prog.push(op.dst as u64);
            prog.push(op.a as u64);
            prog.push(op.b as u64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::analyze_source;
    use crate::kernel;
    use crate::lower::lower_root;

    #[test]
    fn smoke_runs_native_code() {
        // x*3 + 1, computed by rustc-emitted machine code from a MAP_JIT page.
        assert_eq!(smoke(13), Some(40));
        assert_eq!(smoke(0), Some(1));
    }

    /// Lower a source's entry to a kernel, then run it both ways; the JIT must
    /// agree with the interpreter (value, or both poison). `None` from `jit_run`
    /// means the kernel had control flow — not exercised here (all inputs are
    /// straight-line).
    fn diff(src: &str, entry: &str, args: &[Word]) {
        let g = analyze_source(src, entry).expect("analyze");
        let k = lower_root(&g).expect("lower");
        let interp = kernel::run(&k, args);
        let jit = jit_run(&k, args).expect("straight-line kernel should JIT");
        match (jit, interp) {
            (Ok(j), Ok(i)) => assert_eq!(j, i, "{src}: jit {j} != interp {i}"),
            (Err(_), Err(_)) => {} // both poison
            (j, i) => panic!("{src}: jit {j:?} disagrees with interp {i:?}"),
        }
    }

    #[test]
    fn jit_agrees_with_interpreter_on_straight_line_kernels() {
        diff("fn f() -> U64 ( (1 + 2) * 3 )", "f", &[]);
        diff("fn f() -> U64 ( 100 - 58 )", "f", &[]);
        diff("fn f() -> U64 ( 7 % 4 )", "f", &[]);
        diff("fn f() -> Bool ( 3 < 5 )", "f", &[]);
        diff("fn f() -> Bool ( 9 == 9 )", "f", &[]);
        diff("fn f() -> U64 ( (42, 💣)[0] )", "f", &[]); // folds to Const 42
        diff("fn f() -> U64 ( (42, 💣)[1] )", "f", &[]); // folds to bomb → poison
        diff("fn f() -> U64 ( 10 / 0 )", "f", &[]); // divide by zero → poison
    }

    #[test]
    fn jit_agrees_on_parameterised_kernels() {
        diff("fn sq(x: U64) -> U64 ( x * x )", "sq", &[7]);
        diff("fn sq(x: U64) -> U64 ( x * x )", "sq", &[0]);
        diff("fn add1(x: U64) -> U64 ( x + 1 )", "add1", &[41]);
        // identity: a kernel that is just `Return(param)`, no ops.
        diff("fn id(x: U64) -> U64 ( x )", "id", &[123]);
    }

    #[test]
    fn control_flow_kernels_decline_the_jit() {
        // An `if` lowers to multiple blocks; the straight-line JIT returns None and
        // the caller would run the interpreter.
        let src = r#"fn ae(g: U64, w: U64) -> U64 ( if g == w ( 1 ) else ( fail "ne" ) )"#;
        let g = analyze_source(src, "ae").unwrap();
        let k = lower_root(&g).unwrap();
        assert!(
            k.blocks.len() > 1,
            "the `if` kernel should have multiple blocks"
        );
        assert!(
            jit_run(&k, &[1, 1]).is_none(),
            "control flow declines the JIT"
        );
    }
}
