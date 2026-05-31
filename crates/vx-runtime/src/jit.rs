//! The native copy-and-patch backend (rebuild-plan step 5), on Apple Silicon.
//!
//! `kernel.rs` is the **oracle**: a subroutine-threaded interpreter over the same
//! stencils. This module runs the *same* operations as machine code — rustc emits
//! each stencil at build time (`stencils/stencils.rs` → `build.rs`), and here we
//! `memcpy` the stencil bytes into one buffer, patch each op's continuation branch
//! to the next (`copypatch::patch_branch26`), and execute it from `MAP_JIT` memory
//! (`copypatch::ExecBuf`). Same operations, same word ABI, identical results — the
//! differential test asserts `jit_run == kernel::run` on every kernel.
//!
//! **Control flow included.** A kernel is a graph of basic blocks; each block is a
//! stencil chain ending in a terminator stencil — `return`, an unconditional
//! `jump`, or a conditional `branch`. Blocks are laid out in order; the
//! terminators' continuation branches are patched to target blocks
//! (`copypatch::patch_branch26` — all `BRANCH26`, the conditional test stays
//! internal to the `branch` stencil). Operand slot indices and constants ride in a
//! `prog` stream with one region per block; a terminator resets the cursor to its
//! target block's region (`prog_base + offset`) before branching, so the stream
//! stays aligned whichever path runs. Poison (`fail`, divide/remainder by zero) is
//! a status flag + early `ret`, never a panic across the boundary.

use copypatch::{ExecBuf, patch_branch26};

use crate::kernel::{Kernel, Op, OpKind, Poison, Term, Word};
use crate::stencils;

/// Threaded state, matching `Ctx` in `stencils/stencils.rs` byte for byte.
#[repr(C)]
struct Ctx {
    slots: *mut u64,
    prog: *const u64,
    prog_base: *const u64,
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

/// JIT-compile and run a kernel over `args` (bound to slots `0..n_params`).
/// Returns `None` only off the native target; otherwise `Some(result)` where the
/// result is the returned word or a poison — equal to [`kernel::run`].
///
/// [`kernel::run`]: crate::kernel::run
#[must_use]
pub fn jit_run(k: &Kernel, args: &[Word]) -> Option<Result<Word, Poison>> {
    // Off the native target the table is empty — no JIT.
    if stencils::RETURN.is_empty() {
        return None;
    }

    let mut code: Vec<u8> = Vec::new();
    let mut prog: Vec<u64> = Vec::new();
    // Each block's start offset in `code` / `prog`, for resolving cross-block
    // branches and terminator cursor resets once every block is laid out.
    let mut block_code = vec![0usize; k.blocks.len()];
    let mut block_prog = vec![0usize; k.blocks.len()];
    // (code site, target block): a continuation BRANCH26 to patch to the target's
    // code. (prog position, target block): a terminator immediate to fill with the
    // target's prog offset.
    let mut code_patches: Vec<(usize, u32)> = Vec::new();
    let mut prog_fixes: Vec<(usize, u32)> = Vec::new();

    for (b, block) in k.blocks.iter().enumerate() {
        block_code[b] = code.len();
        block_prog[b] = prog.len();

        // A stencil copy + its immediates per op; record starts to chain conts.
        let mut op_starts = Vec::with_capacity(block.ops.len());
        for op in &block.ops {
            op_starts.push(code.len());
            code.extend_from_slice(stencil_bytes(op.kind));
            push_imms(&mut prog, op);
        }

        // The terminator stencil + its immediates.
        let term_start = code.len();
        match block.term {
            Term::Return(slot) => {
                code.extend_from_slice(stencils::RETURN);
                prog.push(slot as u64);
            }
            Term::Jump(t) => {
                code.extend_from_slice(stencils::JUMP);
                prog_fixes.push((prog.len(), t));
                prog.push(0); // target block's prog offset, filled below
                for &rel in stencils::JUMP_CONT {
                    code_patches.push((term_start + rel, t));
                }
            }
            Term::BranchIf {
                cond,
                then_blk,
                else_blk,
            } => {
                code.extend_from_slice(stencils::BRANCH);
                prog.push(cond as u64);
                prog_fixes.push((prog.len(), then_blk));
                prog.push(0);
                prog_fixes.push((prog.len(), else_blk));
                prog.push(0);
                for &rel in stencils::BRANCH_THEN {
                    code_patches.push((term_start + rel, then_blk));
                }
                for &rel in stencils::BRANCH_ELSE {
                    code_patches.push((term_start + rel, else_blk));
                }
            }
        }

        // Within-block: chain each op's cont to the next op (last → terminator).
        for (i, &start) in op_starts.iter().enumerate() {
            let next = op_starts.get(i + 1).copied().unwrap_or(term_start);
            for &rel in stencil_cont(block.ops[i].kind) {
                patch_branch26(&mut code, start + rel, next);
            }
        }
    }

    // Resolve cross-block code branches and terminator prog-offset immediates.
    for &(site, target) in &code_patches {
        patch_branch26(&mut code, site, block_code[target as usize]);
    }
    for &(pos, target) in &prog_fixes {
        prog[pos] = block_prog[target as usize] as u64;
    }

    // Bind args, run from executable memory, read back the result / poison.
    let mut slots = vec![0u64; k.n_slots as usize];
    for (i, &a) in args.iter().take(k.n_params as usize).enumerate() {
        slots[i] = a;
    }
    let buf = ExecBuf::new(&code);
    let base = prog.as_ptr();
    let mut ctx = Ctx {
        slots: slots.as_mut_ptr(),
        prog: base,
        prog_base: base,
        status: 0,
        ret: 0,
    };
    let entry: extern "C" fn(*mut Ctx) = unsafe { core::mem::transmute(buf.as_ptr()) };
    entry(&mut ctx);

    Some(if ctx.status != 0 {
        // The native path can't yet attribute the failing op, so report the
        // kernel's region span (coarse). Per-op fault attribution is a follow-up.
        Err(Poison {
            reason: "kernel poisoned".into(),
            span: k.span,
        })
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
    /// agree with the interpreter — same value, or both poison.
    fn diff(src: &str, entry: &str, args: &[Word]) {
        let g = analyze_source(src, entry).expect("analyze");
        let k = lower_root(&g).expect("lower");
        let interp = kernel::run(&k, args);
        let jit = jit_run(&k, args).expect("native target should JIT");
        match (jit, interp) {
            (Ok(j), Ok(i)) => assert_eq!(j, i, "{src} {args:?}: jit {j} != interp {i}"),
            (Err(_), Err(_)) => {} // both poison
            (j, i) => panic!("{src} {args:?}: jit {j:?} disagrees with interp {i:?}"),
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
        diff("fn id(x: U64) -> U64 ( x )", "id", &[123]); // Return-only, no ops
    }

    #[test]
    fn jit_agrees_on_control_flow_kernels() {
        // assert_eq's shape: the `fail` lowers into the else block; equal args take
        // the then-branch and never reach it, unequal args poison.
        let ae = r#"fn ae(g: U64, w: U64) -> U64 ( if g == w ( 1 ) else ( fail "ne" ) )"#;
        diff(ae, "ae", &[7, 7]);
        diff(ae, "ae", &[7, 8]);

        // A branch with real compute on both sides (a join block carrying on).
        let pick = "fn m(x: U64) -> U64 ( if x < 10 ( x * 2 ) else ( x + 100 ) )";
        diff(pick, "m", &[3]);
        diff(pick, "m", &[50]);

        // Nested `if` (a branch inside a branch): multiple blocks, jumps to joins.
        let nested = "fn n(x: U64) -> U64 ( if x < 5 ( if x < 2 ( 0 ) else ( 1 ) ) else ( 2 ) )";
        diff(nested, "n", &[1]);
        diff(nested, "n", &[3]);
        diff(nested, "n", &[9]);
    }

    #[test]
    fn control_flow_kernels_have_multiple_blocks() {
        // Sanity: the `if` really does lower to a block graph (so the CF path above
        // is exercised, not the single-block one).
        let src = r#"fn ae(g: U64, w: U64) -> U64 ( if g == w ( 1 ) else ( fail "ne" ) )"#;
        let g = analyze_source(src, "ae").unwrap();
        let k = lower_root(&g).unwrap();
        assert!(
            k.blocks.len() > 1,
            "the `if` kernel should have multiple blocks"
        );
    }
}
