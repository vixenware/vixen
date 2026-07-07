# Progressive exec trees — pipelining, check builds, and kill-when-satisfied are one feature

Status: design conversation with Amos, 2026-07-07. Direction endorsed ("this is the goal");
representation details deliberately open. Companion: engine-demand-semantics.md,
vix-substrate-vfs-sandbox.md, the argv-enum/capability-paths ratification (facet-cc
RESURRECTION.md 2026-07-07), and the subfile-reference sketch (facet-cc NEXT.md).

## The claim

rmeta-first pipelining is not a scheduler feature and not a two-step unit split. It is:
**exec output trees materialize incrementally, and subfile demands are satisfied as
subfiles complete, while the process is still running.** Given that, three product
features collapse into one mechanism:

1. **Pipelining** (cargo parity): a dependent that needs only dep's `.rmeta` starts as
   soon as rustc closes that file — not when the process exits.
2. **Kill-when-satisfied** (beyond cargo): if the only standing demands against a unit
   are subfiles it has already produced, the remaining execution is *undemanded work* —
   terminating the process is laziness applied to processes. (Amos: "Rude, yes, but they
   just had to choose a different model.")
3. **Check builds as demand shape**: `vx check` = demand only rmeta subfiles. Codegen is
   never scheduled (tool supports metadata-only) or killed mid-flight (tool doesn't).
   No special casing anywhere.

## Why it's cheap here and expensive elsewhere

Cargo needs rustc's cooperation (JSON artifact-notification side channel). We do not:
**we own the filesystem the process writes into.** vfsd observes writes and closes;
close(2) on `.rmeta` → hash it → release every demand parked on that subfile. The VFS is
the notification channel, tool-agnostic.

Pieces that already exist:
- Parked subfile demands = the ready-token await protocol (weavy task lanes).
- Per-subfile identity = Merkle-shaped ExecTree + DirectoryListingHash(Blake3Hash);
  a subfile's hash is independent of its siblings.
- Read-set entries at subfile granularity = vfsd File/Directory/LookupMiss observations.

## Semantics that fall out (not bolted on)

- **Memo consistency under kill**: a killed unit's memo holds entries for subfiles it
  produced. A later demand for an unproduced subfile (.rlib) is a memo miss on that
  subfile → rerun the unit. The fresh .rmeta must hash identical to the recorded one —
  the receipt VERIFIES it, turning rustc reproducibility from assumption into a
  continuously checked invariant (loud failure on mismatch).
- **Close-is-provisional**: a process may reopen a file; treat close as provisional-final,
  verify hash unchanged at exit. A tool that violates it fails loudly — same posture as
  undeclared reads.
- **Invalidation vs execution granularity**: execution stays per-unit (can't run half a
  process — but can kill one); invalidation propagates per-subfile (rmeta-unchanged ⇒
  rmeta-only consumers memo-hit: the RDR decision half, expressed in the demand model).

## Sequencing

After the argv-enum + capability-paths mission (facet-cc NEXT.md) introduces subfile
references. Joint vix/vixenware design: ExecTree grows progressive updates in the
vx-services protocol; the machine's exec seam grows subfile futures. rmeta-first split
emission (frontend-once) is a later refinement of the same shape, not a prerequisite.
