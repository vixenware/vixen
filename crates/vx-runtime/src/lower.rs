//! Extraction: a maximal pure **scalar** region of the demand graph → kernel IR.
//!
//! rebuild-plan step (3). `analyze.rs` classifies every node; this pass takes the
//! region of `Kernel`/`Const`/`Param` nodes anchored at the demand root — folding
//! projections through known constructors, lowering `if` to basic blocks + a
//! conditional branch — and produces a [`Kernel`] the interpreter (and, later, the
//! JIT) runs. It reads the **structured** [`KernelOp`]/[`BinKind`] the analysis
//! recorded, never the human label: one total `match`, checked by the compiler.
//!
//! **Honest and partial.** It lowers the scalar core: integer/bool constants,
//! parameters, scalar arithmetic/comparison/logic, `not`, `if`, and the poisons
//! (`💣`/`fail`). Everything else — string/compound values, `++`/`|>`, calls (no
//! inlining yet), boundaries, lazy-data *as a value* — returns [`NotLowerable`],
//! and the evaluator falls back to tree-walking (`eval.rs`). It never silently
//! miscompiles. The lowerable set grows example by example: the build fixtures
//! bring boundaries, which become multi-region extraction (each boundary result
//! threaded in as a parameter).
//!
//! `eval.rs` is the **oracle**: for any pure no-arg body, `run(lower_root(g))`
//! equals `eval::run` — the test at the bottom asserts exactly that.

use std::collections::HashSet;

use crate::graph::{BinKind, DemandGraph, KernelOp, Node, NodeId, NodeKind};
use crate::kernel::{Block, Kernel, Op, OpKind, Slot, Term, Word};

/// Why a region wasn't lowered to the scalar kernel IR — the kind of node that
/// stopped it. Not an error: the signal for the evaluator to tree-walk instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotLowerable(pub String);

/// Lower the maximal pure scalar region anchored at the demand root to one kernel.
/// `Err` means the region isn't scalar-lowerable yet.
pub fn lower_root(g: &DemandGraph) -> Result<Kernel, NotLowerable> {
    lower_node(g, g.root).map(|(kernel, _)| kernel)
}

/// Lower the maximal pure scalar region anchored at **any** node (not just the
/// demand root) to a kernel, also returning the region's parameter **nodes** in
/// slot order: slot `i` reads the value of `params[i]`. The evaluator maps each
/// parameter node back to the binding it came from to supply the argument — so a
/// scalar *sub-expression* of a body can run as a kernel, its free variables fed
/// from the environment.
pub fn lower_node(g: &DemandGraph, root: NodeId) -> Result<(Kernel, Vec<NodeId>), NotLowerable> {
    let mut lo = Lowerer {
        g,
        params: Vec::new(),
        n_slots: 0,
        blocks: Vec::new(),
        cur: Vec::new(),
    };
    // Phase 1: discover the parameter nodes (region inputs) and give them the
    // first slots — the kernel ABI binds args to slots `0..n_params`.
    let mut seen = HashSet::new();
    lo.collect_params(root, &mut seen)?;
    lo.n_slots = lo.params.len() as u32;
    // Phase 2: emit the region, then return its root value.
    let root_slot = lo.emit_value(root)?;
    lo.close(Term::Return(root_slot));
    let span = lo.node(root).span;
    Ok((
        Kernel {
            n_slots: lo.n_slots,
            n_params: lo.params.len() as u32,
            blocks: lo.blocks,
            span,
        },
        lo.params,
    ))
}

struct Lowerer<'g> {
    g: &'g DemandGraph,
    /// Region-input nodes, in slot order: param `i` occupies slot `i`.
    params: Vec<NodeId>,
    n_slots: u32,
    /// Finalised blocks; block 0 is the entry.
    blocks: Vec<Block>,
    /// The block currently being built (not yet pushed to `blocks`).
    cur: Vec<Op>,
}

impl<'g> Lowerer<'g> {
    /// Fold projections through known constructors, so `pair[0]` resolves to the
    /// field node directly — the same case-of-known-constructor fold the demand
    /// walk uses.
    fn resolve(&self, n: NodeId) -> NodeId {
        self.g.resolved_value(n).unwrap_or(n)
    }

    /// A node, borrowed for the *graph's* lifetime (not `&self`'s) — so we can read
    /// it while emitting through `&mut self`.
    fn node(&self, n: NodeId) -> &'g Node {
        &self.g.nodes[n.0 as usize]
    }

    fn alloc(&mut self) -> Slot {
        let s = self.n_slots;
        self.n_slots += 1;
        s
    }

    /// Finalise the open block with `term`, returning its index. Leaves `cur`
    /// empty, ready for the next block.
    fn close(&mut self, term: Term) -> u32 {
        let idx = self.blocks.len() as u32;
        let ops = std::mem::take(&mut self.cur);
        self.blocks.push(Block { ops, term });
        idx
    }

    /// Phase 1: collect the region's parameter nodes (first-encountered order),
    /// erroring if any node on the way isn't scalar-lowerable.
    fn collect_params(
        &mut self,
        n: NodeId,
        seen: &mut HashSet<NodeId>,
    ) -> Result<(), NotLowerable> {
        let n = self.resolve(n);
        if !seen.insert(n) {
            return Ok(());
        }
        let node = self.node(n);
        match &node.kind {
            NodeKind::Param => {
                if !self.params.contains(&n) {
                    self.params.push(n);
                }
                Ok(())
            }
            // Const / 💣 / fail: leaves (fail's message is ignored by the Fail op).
            NodeKind::Const | NodeKind::Bomb | NodeKind::Fail => Ok(()),
            NodeKind::Kernel(KernelOp::Bin(_)) => {
                let (a, b) = (node.inputs[0], node.inputs[1]);
                self.collect_params(a, seen)?;
                self.collect_params(b, seen)
            }
            NodeKind::Kernel(KernelOp::Not) => self.collect_params(node.inputs[0], seen),
            NodeKind::Kernel(KernelOp::If) => {
                if node.inputs.len() < 3 {
                    return Err(NotLowerable("`if` without an `else` branch".into()));
                }
                let (c, t, e) = (node.inputs[0], node.inputs[1], node.inputs[2]);
                self.collect_params(c, seen)?;
                self.collect_params(t, seen)?;
                self.collect_params(e, seen)
            }
            other => Err(NotLowerable(format!("{other:?}"))),
        }
    }

    /// Phase 2: emit ops computing `n`'s value, returning its slot. Straight-line
    /// ops append to the open block; `if` splits it (short-circuit).
    fn emit_value(&mut self, n: NodeId) -> Result<Slot, NotLowerable> {
        let n = self.resolve(n);
        let node = self.node(n);
        match &node.kind {
            NodeKind::Param => Ok(self
                .params
                .iter()
                .position(|&p| p == n)
                .expect("param was collected in phase 1")
                as Slot),

            NodeKind::Const => {
                let imm = const_word(&node.label)?;
                let dst = self.alloc();
                self.cur.push(Op {
                    kind: OpKind::Const,
                    dst,
                    a: 0,
                    b: 0,
                    imm,
                    span: node.span,
                });
                Ok(dst)
            }

            // 💣 forced in a live region, or explicit `fail`: poison. The Fail op
            // aborts the run, so the slot it "returns" is never actually read.
            NodeKind::Bomb | NodeKind::Fail => {
                let dst = self.alloc();
                self.cur.push(Op {
                    kind: OpKind::Fail,
                    dst,
                    a: 0,
                    b: 0,
                    imm: 0,
                    span: node.span,
                });
                Ok(dst)
            }

            NodeKind::Kernel(KernelOp::Bin(bin)) => {
                let kind = bin_op(*bin);
                let span = node.span;
                let (an, bn) = (node.inputs[0], node.inputs[1]);
                let a = self.emit_value(an)?;
                let b = self.emit_value(bn)?;
                let dst = self.alloc();
                self.cur.push(Op {
                    kind,
                    dst,
                    a,
                    b,
                    imm: 0,
                    span,
                });
                Ok(dst)
            }

            NodeKind::Kernel(KernelOp::Not) => {
                let span = node.span;
                let an = node.inputs[0];
                let a = self.emit_value(an)?;
                let dst = self.alloc();
                self.cur.push(Op {
                    kind: OpKind::Not,
                    dst,
                    a,
                    b: 0,
                    imm: 0,
                    span,
                });
                Ok(dst)
            }

            NodeKind::Kernel(KernelOp::If) => {
                if node.inputs.len() < 3 {
                    return Err(NotLowerable("`if` without an `else` branch".into()));
                }
                let (c, t, e) = (node.inputs[0], node.inputs[1], node.inputs[2]);
                self.emit_if(c, t, e)
            }

            other => Err(NotLowerable(format!("{other:?}"))),
        }
    }

    /// Lower `if cond ( then ) else ( else )` to blocks + a conditional branch.
    /// Both branches write one shared `result` slot (the de-SSA'd φ) and jump to a
    /// join block. Only the taken side runs — a bomb lowered into the dead branch
    /// never detonates: the proof the partition survives lowering.
    fn emit_if(&mut self, cond: NodeId, then: NodeId, els: NodeId) -> Result<Slot, NotLowerable> {
        let cond_slot = self.emit_value(cond)?;
        let result = self.alloc();
        // Close the predecessor block; its branch targets are patched once known.
        let pre = self.close(Term::Return(cond_slot));

        let then_idx = self.blocks.len() as u32;
        let t = self.emit_value(then)?;
        self.cur.push(Op {
            kind: OpKind::Copy,
            dst: result,
            a: t,
            b: 0,
            imm: 0,
            span: None, // synthetic φ move, not a source op
        });
        let then_end = self.close(Term::Return(result));

        let else_idx = self.blocks.len() as u32;
        let e = self.emit_value(els)?;
        self.cur.push(Op {
            kind: OpKind::Copy,
            dst: result,
            a: e,
            b: 0,
            imm: 0,
            span: None, // synthetic φ move, not a source op
        });
        let else_end = self.close(Term::Return(result));

        // `cur` is now the (empty) join block, open for subsequent ops.
        let join = self.blocks.len() as u32;
        self.blocks[pre as usize].term = Term::BranchIf {
            cond: cond_slot,
            then_blk: then_idx,
            else_blk: else_idx,
        };
        self.blocks[then_end as usize].term = Term::Jump(join);
        self.blocks[else_end as usize].term = Term::Jump(join);
        Ok(result)
    }
}

/// A scalar constant's word value, parsed from the literal's raw text (the same
/// raw `vx_ir` stores and `eval.rs` parses). `Err` for non-scalar literals
/// (string, float) — they aren't single words.
fn const_word(label: &str) -> Result<Word, NotLowerable> {
    if let Ok(n) = label.parse::<u64>() {
        return Ok(n);
    }
    match label {
        "true" => Ok(1),
        "false" => Ok(0),
        other => Err(NotLowerable(format!("non-scalar constant `{other}`"))),
    }
}

/// Map the graph's scalar [`BinKind`] to the executable [`OpKind`].
fn bin_op(b: BinKind) -> OpKind {
    match b {
        BinKind::Add => OpKind::Add,
        BinKind::Sub => OpKind::Sub,
        BinKind::Mul => OpKind::Mul,
        BinKind::Div => OpKind::Div,
        BinKind::Rem => OpKind::Rem,
        BinKind::Eq => OpKind::Eq,
        BinKind::Ne => OpKind::Ne,
        BinKind::Lt => OpKind::Lt,
        BinKind::Le => OpKind::Le,
        BinKind::Gt => OpKind::Gt,
        BinKind::Ge => OpKind::Ge,
        BinKind::And => OpKind::And,
        BinKind::Or => OpKind::Or,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::analyze_source;
    use crate::eval::{self, Outcome, Value};
    use crate::kernel;

    fn lower_run(src: &str, entry: &str, args: &[Word]) -> Result<Word, String> {
        let g = analyze_source(src, entry).map_err(|e| format!("analyze: {e}"))?;
        let k = lower_root(&g).map_err(|e| format!("lower: {e:?}"))?;
        kernel::run(&k, args).map_err(|p| format!("poison: {}", p.reason))
    }

    #[test]
    fn arithmetic_extracts_and_runs() {
        // (1 + 2) * 3 turned inside out, lowered, and run → 9 (cf. the hand-built
        // kernel in kernel.rs — now produced from real source).
        assert_eq!(lower_run("fn f() -> U64 ( (1 + 2) * 3 )", "f", &[]), Ok(9));
    }

    #[test]
    fn projection_folds_to_the_live_field_only() {
        // (42, 💣)[0] lowers to just `Const 42` — the bomb never becomes an op.
        assert_eq!(lower_run("fn f() -> U64 ( (42, 💣)[0] )", "f", &[]), Ok(42));
        // (42, 💣)[1] folds to the bomb → a Fail op → poison (no value).
        assert!(lower_run("fn f() -> U64 ( (42, 💣)[1] )", "f", &[]).is_err());
    }

    #[test]
    fn if_extracts_to_a_branch_and_short_circuits() {
        // A monomorphic assert_eq body: `if got == want ( 1 ) else ( fail )`.
        // The `fail` lowers into the *else* block; for equal args it never runs.
        let src =
            r#"fn ae(got: U64, want: U64) -> U64 ( if got == want ( 1 ) else ( fail "ne" ) )"#;
        assert_eq!(lower_run(src, "ae", &[42, 42]), Ok(1)); // then-branch
        assert!(lower_run(src, "ae", &[42, 43]).is_err()); // else-branch: fail poisons
    }

    #[test]
    fn if_in_subexpression_rejoins() {
        // The `if` is an operand, not the tail — proves the join block carries on:
        // (if 1 < 2 ( 10 ) else ( 20 )) + 5  →  15.
        let src = "fn f() -> U64 ( (if 1 < 2 ( 10 ) else ( 20 )) + 5 )";
        assert_eq!(lower_run(src, "f", &[]), Ok(15));
    }

    #[test]
    fn param_used_twice_is_one_slot() {
        // x * x with x = 7 → 49; the single Param node maps to one slot, read twice.
        assert_eq!(
            lower_run("fn sq(x: U64) -> U64 ( x * x )", "sq", &[7]),
            Ok(49)
        );
    }

    #[test]
    fn opaque_ops_decline_lowering() {
        // A call isn't inlined yet → NotLowerable (honest), so the evaluator would
        // tree-walk. (string `++` likewise.)
        let g = analyze_source("fn f() -> U64 ( g(1) ) fn g(x: U64) -> U64 ( x )", "f").unwrap();
        assert!(lower_root(&g).is_err());
    }

    /// I6 — Source fidelity: a poison reports its source span, and the spanned
    /// text is the token that detonated (divide-by-zero, `fail`, a forced `💣`).
    #[test]
    fn i6_poison_carries_its_source_span() {
        for (src, marker) in [
            ("fn f() -> U64 ( 10 / 0 )", "/"),
            (r#"fn f() -> U64 ( fail "boom" )"#, "fail"),
            ("fn f() -> U64 ( (42, 💣)[1] )", "💣"),
        ] {
            let g = analyze_source(src, "f").expect("analyze");
            let k = lower_root(&g).expect("lower");
            let span = match kernel::run(&k, &[]) {
                Err(p) => p
                    .span
                    .unwrap_or_else(|| panic!("{src}: poison carried no span")),
                Ok(v) => panic!("{src}: expected poison, got {v}"),
            };
            let text = src
                .get(span.start as usize..span.end as usize)
                .unwrap_or_else(|| panic!("{src}: span {span:?} out of bounds"));
            assert!(
                text.contains(marker),
                "{src}: span text {text:?} should contain {marker:?}"
            );
        }
    }

    /// `eval.rs` is the oracle: lowering must agree with the tree-walker on every
    /// pure no-arg scalar body — same value, or both poison.
    #[test]
    fn lowering_agrees_with_the_evaluator() {
        let cases = [
            "fn f() -> U64 ( (1 + 2) * 3 )",
            "fn f() -> U64 ( 100 - 58 )",
            "fn f() -> U64 ( 7 % 4 )",
            "fn f() -> Bool ( 3 < 5 )",
            "fn f() -> Bool ( 3 == 3 )",
            "fn f() -> U64 ( if 2 > 1 ( 8 ) else ( 9 ) )",
            "fn f() -> U64 ( (42, 💣)[0] )",
            "fn f() -> U64 ( (42, 💣)[1] )", // both poison
            "fn f() -> U64 ( 10 / 0 )",      // both poison (divide by zero)
        ];
        for src in cases {
            let oracle = match eval::run(src, "f").unwrap() {
                Outcome::Clean(Value::U64(n)) => Ok(n),
                Outcome::Clean(Value::Bool(b)) => Ok(b as Word),
                Outcome::Clean(other) => panic!("{src}: non-scalar value {other:?}"),
                Outcome::Poisoned { .. } => Err(()),
            };
            let lowered = lower_run(src, "f", &[]).map_err(|_| ());
            assert_eq!(
                lowered, oracle,
                "lowering disagrees with the evaluator on `{src}`"
            );
        }
    }
}
