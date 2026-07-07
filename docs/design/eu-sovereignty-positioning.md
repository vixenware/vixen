# EU sovereignty positioning — the business thesis

Status: Amos-stated thesis, 2026-07-07 ("the thing that's gonna give us millions this
year"). This document is the technical side's grounding; the business conversation runs
in parallel (other Fable lane). Companion: package-manager-and-provenance.md.

## The thesis (Amos, verbatim intent)

Short CI times and remote multi-platform builds are the love; **European sovereignty,
traceability, and auditability are the money.** The political climate is funding digital
sovereignty at state scale — Amos cites ~€13B assembled by France alone for this (France
2030 envelope; verify exact figures before putting them in front of anyone). If the
sovereignty/traceability/audit story is real on day one, it funds everything else.

Consequences Amos named explicitly:
- Priorities shift: get something building, yes — but presentation, website, and BRANDING
  are now first-class workstreams.
- **Two-brand architecture**: vix + the fox stay the language's identity forever
  ("it'll always live in my heart"). The institutional product may need a different
  name — one a European ministry can write into a procurement document ("maybe it's
  called fucking MetalGraph or whatever"). Naming is OPEN, not decided; a committee
  round with EU-procurement personas is the vetted next step when Amos calls for it
  (personas: French procurement officer; CISO staring at the CRA deadline; sovereignty-
  fund evaluator).

## Regulatory tailwind → mechanism mapping

The demand is legislated, dated, and unfunded-mandate-shaped (verify all dates/details
against current texts before external use; this is engineering memory, not legal advice):

| Regulation | What it demands | What we answer with |
|---|---|---|
| **CRA** (Reg. 2024/2847; in force Dec 2024; vuln-reporting obligations from Sep 2026; full application Dec 2027) | Know what's in every product; SBOM in technical docs; vulnerability handling + 24h ENISA early-warning on actively exploited vulns | Recipe-embedded artifacts = SBOM that cannot drift (hermetic: undeclared input is a build FAILURE, not an omission); hash-keyed advisories = "is the vulnerable code in anything we ship?" answered exactly, fleet-wide, from the artifacts themselves |
| **NIS2** (supply-chain security duties, critical sectors) | Demonstrate supply-chain risk management | Receipts = the build's own decision trace, complete by construction; transparency log = append-only publish history; overlay receipts = provenance of every patch |
| **DORA** (financial ICT) | Auditability of the software estate | Same receipts story; `vx audit <dir>` on deployed artifacts |
| **SecNumCloud / cloud de confiance** (FR procurement) | Qualified EU-jurisdiction hosting | Registry + cache operated by vixenware (EU corp) on EU infra; SecNumCloud-qualified hosting is the flex to investigate |

The sharp differentiator, phrased once: **SLSA provenance is a signed claim about what a
build did; a vixen receipt is the observed read-set. Compliance stops being paperwork and
becomes a build-system property.** The Sep-2026 CRA reporting deadline means every EU
software vendor needs vulnerability-handling machinery NOW — the urgency is theirs, not
ours to manufacture.

## Funding landscape (names to research properly, not from memory)

France 2030 (the ~€13B Amos cites), Sovereign Tech Agency (DE — funds open digital
infrastructure; most of our stack is destined open source, which FITS), EIC Accelerator,
EuroStack initiative. Note the cautionary tale: Gaia-X (sovereignty branding without a
product). Our counter-position: the product exists and the sovereignty properties are
structural, not certificational.

## Website implications (in progress, my lane)

- User site gets a second entry lane beside "my CI is slow": **"I have to prove what's
  in my software"** → /provenance/ (done 2026-07-07: provenance.md + _index lanes).
- proof.md and receipts pages are load-bearing for this audience — an auditor's first
  question is "show me," and we uniquely can.
- Disclaimer discipline holds and MATTERS MORE here: funders diligence claims. Real-today
  vs designed-spec is always marked, /status/ is the dated ledger.
- Branding/naming and any visual identity shift: NOT started without Amos — committee
  round on call.
