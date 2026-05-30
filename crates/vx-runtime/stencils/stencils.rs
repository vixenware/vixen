//! vx-runtime kernel stencils, in Rust. `build.rs` compiles this to an object with
//! rustc (`--emit=obj`) and extracts each stencil's machine code — and its
//! `vx_cont` relocation — by symbol. These are the **same** operations as
//! `src/kernel.rs`'s interpreter, in the copy-and-patch form: the interpreter is
//! the oracle, this is what runs.
//!
//! Each stencil threads state through `*mut Ctx` and reaches the next op by
//! tail-calling the external `vx_cont`; that branch's `BRANCH26` relocation is the
//! hole `build.rs`/the JIT patches to chain copies. Per-op operands (slot indices)
//! and the `Const` immediate ride in `Ctx.prog`, consumed in order — so one `add`
//! stencil serves every `add`, immediate-agnostic (phon's flavour: copy the code,
//! patch the control flow, thread the data).
//!
//! With `--cfg tailcall` (nightly) the continuation is a guaranteed tail call
//! (`become`): the whole chain runs in one frame as a series of jumps. Without it
//! (stable) it falls back to an ordinary call; both extract identically.
//!
//! Scalars are **unboxed**: a slot *is* the `u64`. Poison (`fail`, divide-by-zero)
//! is a returned status, never a panic across a stencil boundary — so the file
//! compiles with `panic=abort` and never unwinds.

#![cfg_attr(tailcall, feature(explicit_tail_calls))]
#![allow(clippy::missing_safety_doc)]

/// Threaded state, matching `jit::Ctx` byte for byte.
#[repr(C)]
pub struct Ctx {
    /// The kernel's slot array (params pre-filled); scalars unboxed into it.
    pub slots: *mut u64,
    /// The immediate stream: each op's operand slot indices, then `Const`'s value.
    pub prog: *const u64,
    /// 0 = ok, 1 = poison (`fail`, divide/remainder by zero).
    pub status: u64,
    /// The returned word, written by `vx_stencil_return`.
    pub ret: u64,
}

extern "C" {
    fn vx_cont(cx: *mut Ctx);
}

/// Tail-call the continuation (the patched `vx_cont` BRANCH26 hole).
macro_rules! cont {
    ($cx:expr) => {{
        #[cfg(tailcall)]
        become vx_cont($cx);
        #[cfg(not(tailcall))]
        vx_cont($cx);
    }};
}

/// `smoke(x) = x*3 + 1`, a self-contained stencil with no relocations: the canary
/// that machine code copied into `MAP_JIT` memory runs on this host. The real
/// stencils plug into the same substrate.
#[no_mangle]
pub extern "C" fn vx_stencil_smoke(x: u64) -> u64 {
    x.wrapping_mul(3).wrapping_add(1)
}

/// `slots[dst] = imm` — prog `[dst, imm]`.
#[no_mangle]
pub unsafe extern "C" fn vx_stencil_const(cx: *mut Ctx) {
    let c = &mut *cx;
    let dst = *c.prog as usize;
    let imm = *c.prog.add(1);
    c.prog = c.prog.add(2);
    *c.slots.add(dst) = imm;
    cont!(cx);
}

/// `slots[dst] = slots[a]` — prog `[dst, a]`. The de-SSA'd φ move.
#[no_mangle]
pub unsafe extern "C" fn vx_stencil_copy(cx: *mut Ctx) {
    let c = &mut *cx;
    let dst = *c.prog as usize;
    let a = *c.prog.add(1) as usize;
    c.prog = c.prog.add(2);
    *c.slots.add(dst) = *c.slots.add(a);
    cont!(cx);
}

/// One binary stencil: prog `[dst, a, b]`, `slots[dst] = body(slots[a], slots[b])`,
/// then continue. The body is inlined (no indirect call) so the stencil is just
/// the operation.
macro_rules! bin {
    ($name:ident, |$a:ident, $b:ident| $body:expr) => {
        #[no_mangle]
        pub unsafe extern "C" fn $name(cx: *mut Ctx) {
            let c = &mut *cx;
            let dst = *c.prog as usize;
            let ai = *c.prog.add(1) as usize;
            let bi = *c.prog.add(2) as usize;
            c.prog = c.prog.add(3);
            let $a = *c.slots.add(ai);
            let $b = *c.slots.add(bi);
            *c.slots.add(dst) = $body;
            cont!(cx);
        }
    };
}

bin!(vx_stencil_add, |a, b| a.wrapping_add(b));
bin!(vx_stencil_sub, |a, b| a.wrapping_sub(b));
bin!(vx_stencil_mul, |a, b| a.wrapping_mul(b));
bin!(vx_stencil_eq, |a, b| (a == b) as u64);
bin!(vx_stencil_ne, |a, b| (a != b) as u64);
bin!(vx_stencil_lt, |a, b| (a < b) as u64);
bin!(vx_stencil_le, |a, b| (a <= b) as u64);
bin!(vx_stencil_gt, |a, b| (a > b) as u64);
bin!(vx_stencil_ge, |a, b| (a >= b) as u64);
bin!(vx_stencil_and, |a, b| ((a != 0) && (b != 0)) as u64);
bin!(vx_stencil_or, |a, b| ((a != 0) || (b != 0)) as u64);

/// `slots[dst] = (slots[a] == 0)` — prog `[dst, a]`. Logical negation.
#[no_mangle]
pub unsafe extern "C" fn vx_stencil_not(cx: *mut Ctx) {
    let c = &mut *cx;
    let dst = *c.prog as usize;
    let a = *c.prog.add(1) as usize;
    c.prog = c.prog.add(2);
    *c.slots.add(dst) = (*c.slots.add(a) == 0) as u64;
    cont!(cx);
}

/// `slots[dst] = slots[a] / slots[b]` — prog `[dst, a, b]`. Divide by zero poisons
/// (status 1) and stops; no panic across the boundary.
#[no_mangle]
pub unsafe extern "C" fn vx_stencil_div(cx: *mut Ctx) {
    let c = &mut *cx;
    let dst = *c.prog as usize;
    let ai = *c.prog.add(1) as usize;
    let bi = *c.prog.add(2) as usize;
    c.prog = c.prog.add(3);
    let b = *c.slots.add(bi);
    if b == 0 {
        c.status = 1;
        return;
    }
    *c.slots.add(dst) = *c.slots.add(ai) / b;
    cont!(cx);
}

/// `slots[dst] = slots[a] % slots[b]` — prog `[dst, a, b]`. Remainder by zero
/// poisons (status 1) and stops.
#[no_mangle]
pub unsafe extern "C" fn vx_stencil_rem(cx: *mut Ctx) {
    let c = &mut *cx;
    let dst = *c.prog as usize;
    let ai = *c.prog.add(1) as usize;
    let bi = *c.prog.add(2) as usize;
    c.prog = c.prog.add(3);
    let b = *c.slots.add(bi);
    if b == 0 {
        c.status = 1;
        return;
    }
    *c.slots.add(dst) = *c.slots.add(ai) % b;
    cont!(cx);
}

/// `fail` — poison unconditionally (status 1) and stop. No operands, no
/// continuation: just a `ret`.
#[no_mangle]
pub unsafe extern "C" fn vx_stencil_fail(cx: *mut Ctx) {
    (*cx).status = 1;
}

/// `return slots[a]` — prog `[a]`. The terminator: write the result word and
/// return to the driver (no continuation).
#[no_mangle]
pub unsafe extern "C" fn vx_stencil_return(cx: *mut Ctx) {
    let c = &mut *cx;
    let a = *c.prog as usize;
    c.prog = c.prog.add(1);
    c.ret = *c.slots.add(a);
}
