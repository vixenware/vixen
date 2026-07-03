# Brief: rebase snark-playground-rebased onto origin/main

*Delegation prompt. DO NOT START while any other agent is working in
~/oss/facet-cc — this rewrites the branch's history. Repo: ~/oss/facet-cc.*

## Situation

- `origin/main` is ~110 commits ahead of our merge-base: PL-core's snark/
  weavy performance work (reuse indexes, goto rows, journal shrinking) plus
  field-aware highlight fixes.
- `snark-playground-rebased` carries our ~34+ commits: the vix language
  (vix/, vix-wire/, vix-daemon/ — all ADDITIVE, new crates), snark grammar
  bundle for vix (playgrounds/snark/src/bundled/vix/ — additive), snark-wasm
  additions (vixBindings/vixHighlights), vox additions (WsListener upgrade
  fix, channel tests), weavy async stencils, and ONE change inside snark
  core: 329f5798e "snark: propagate field() through hidden reductions".

## Procedure

1. `git fetch origin`, confirm no other agent has the checkout dirty
   (`git status` clean; if not, STOP and report).
2. `git rebase origin/main` on snark-playground-rebased.
3. Conflict rules:
   - Cargo.lock: take THEIRS then `cargo build -p vix` to regenerate our
     additions into it.
   - vix/, vix-wire/, vix-daemon/, playgrounds/snark/src/bundled/vix/:
     ours (additive; conflicts here should be rare and mechanical).
   - weavy/src/jit/mod.rs: keep BOTH — our async_stencils cfg(feature=
     "jit") module additions layered onto whatever they changed.
   - playgrounds/snark/src/App.tsx: keep our vix-related and no-italics
     changes layered onto theirs; if a hunk is unintelligible, prefer
     theirs and re-apply ours minimally, then `pnpm run build` in
     playgrounds/snark must pass.
   - snark/src/lower/weavy.rs (THE judgment call): upstream refactored
     this file heavily and added their own field fixes. The EMPIRICAL
     rule: our fix exists because vix's lowering needs field() propagated
     through repeat auxiliaries — without it, vix tests fail. First try
     resolving to THEIR version entirely (drop our hunks); run
     `cargo nextest run -p vix`. If green, our fix was subsumed — record
     that in the commit message of the rebased commit (or drop the commit
     with `git rebase --skip` and note it). If red, re-apply the INTENT of
     our fix onto their refactored code (the take_field_events re-queue on
     hidden-fragment pop) until vix tests pass.
4. Full verification: `cargo nextest run -p vix -p vix-daemon -p vix-wire
   -p vox -p snark` (default profile), `cargo clippy -p vix -p vix-daemon
   -p vix-wire --all-targets` warning-free, and `cargo build -p snark-wasm
   --target wasm32-unknown-unknown --release`.
5. `git push --force-with-lease origin snark-playground-rebased`.

## Report

Old and new branch head hashes; per-file conflict resolutions actually
taken; the weavy.rs verdict (subsumed or re-applied, with evidence);
full test summary.
