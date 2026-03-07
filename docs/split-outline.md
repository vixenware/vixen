# Split Outline: design-claude.md

Mapping from the current monolithic document into split files.
Each target file lists the source sections it draws from.

---

## 00-objective.md

**Purpose**: What Stage 0 is trying to validate and what it is not.

**Source sections**:
- "Objective: what this phase is trying to validate" (formerly §0.1)

---

## 01-invariants.md

**Purpose**: Architectural invariants that must hold for correctness. The contract that all other documents must satisfy.

**Source sections**:
- "Architectural invariants" (12 numbered invariants)

---

## 02-semantic-core.md

**Purpose**: Normative semantic model — the language's meaning, identity rules, and caching semantics. The largest file.

**Source sections**:
- Glossary
- §1. Motivation and framing (§1.1 problem, §1.2 prior art, §1.3 BSaLC positioning)
- §2. Core semantic model (§2.1–§2.7: programs, values, pure/host computation, dynamic dependencies, determinism, exec semantics, semantic vs telemetry)
- §4. Cache and identity model (§4.1–§4.8: identity taxonomy, function identity, late-bound inputs, value-based caching, memoization invariants, memo entry structure and value storage)
- §5. Output and artifact model (§5.1–§5.4: store objects vs language values, trees, streams, typed exec outputs)
- §7. Incrementality and invalidation (§7.1–§7.3: triggers, file watching, early cutoff / dependency traces / cache validity)

**Notes**:
- §5.3 (Streams) carries a stage note: "not part of Stage 0"
- §4.8 (memo entry structure) defines the two-class value storage model
- §7.3 defines the two-phase cache validity check (trace verification + early cutoff)

---

## 03-operational-core.md

**Purpose**: Runtime architecture, process model, sandboxing, store lifecycle, failure recovery. How the semantic model is realized.

**Source sections**:
- §3. Operational model (§3.1–§3.5: architecture, Model A/B split, sandboxing/VFS, remote execution, backpressure)
- §6. Store / GC / liveness model (§6.1–§6.3: three states, leases, roots/reachability)
- §8. Failure, cancellation, and crash recovery (§8.1–§8.5: executor death, partial uploads, daemon absent, interruption, correctness guarantees)

---

## 04-example-corpus/

**Purpose**: Worked examples that demonstrate the semantic and operational model against concrete build scenarios.

**Source sections → files**:
- §4.6 "Value-based caching, concretely" → `early-cutoff-toml.md`
- §5.5 "Why generic exec is not a semantic stream" → `exec-vs-stream.md`
- §6.4 "Worked example: downloading and extracting a Rust toolchain" → `acquire-toolchain.md`
- §7.4 "Concrete example: Rust crate with dynamically discovered tests" → `dynamic-test-discovery.md`

**Notes**:
- Each example file should be self-contained: include enough context to understand the example without reading the full spec
- Examples reference normative rules from 02-semantic-core.md by section number

---

## 05-stage0-blockers.md

**Purpose**: Questions that must be answered before the first evaluator can be implemented, plus the MVP scope definition.

**Source sections**:
- "Unresolved questions" → "Stage 0 blockers" subsection (items 1–6)
- "Proposed MVP cut" → "Specifically included", "Memoization scope in Stage 0", "Remote compatibility", "Specifically excluded from MVP"

---

## 06-decisions.md

**Purpose**: Decisions already made, implementation staging, and open questions that do not block Stage 0.

**Source sections**:
- §9. Implementation plan (§9.1: staged plan, Stages 0–4)
- §10. Open questions and risks (§10.1 design questions not blocking Stage 0, §10.2 risks, §10.3 collapse points / decisions made)
- "Unresolved questions" → "Later open questions" subsection (items 7–12)

---

## 07-non-goals.md

**Purpose**: Things the system does not attempt. Permanent scope boundaries, not deferred features.

**Source sections**:
- "Non-goals" (formerly §0)

---

## Not split (non-normative)

**Appendix A: Current implementation assets** — remains in design-claude.md or is dropped from the split entirely. It is implementation context, not architecture. If included, it should be clearly labeled non-normative and kept as a standalone reference, not bundled into any normative file.
