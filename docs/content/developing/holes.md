+++
title = "Deliberate holes"
+++

Some things in these repositories are missing **on purpose**, and the project
treats that as a first-class technique. This page exists so you recognize the
pattern instead of "fixing" it.

The reasoning: in a project where much implementation is done by AI agents —
and where humans, too, mostly learn a codebase by reading it — **existing code
is authority**. A wrong-shaped implementation doesn't just fail to help; it
actively recruits every future reader into extending the wrong shape. Deleting
it is not losing work. It is *removing false testimony*.

Two canonical instances:

**The resolver's reference implementation.** rodin was first built in Rust to
prove the algorithms and measure against Cargo. Once distilled into
specification documents, the Rust implementation was deleted — because agents
kept mistaking the most-complete-looking code for the main line, when the
actual goal was the vix implementation. The specs and the differential
fixtures remained; the code that pointed the wrong way did not. ("Cargo is
the oracle; the docs are the spec.")

**The daemon.** Three implementations existed over time; none had the shape
the architecture needed. All three were removed, leaving a visible gap where
a daemon obviously belongs — so that the next implementation would be built
from the *intended* architecture (recorded in prose) rather than pattern-
matched from the last wrong one.

The rules when you find a hole:

1. **Don't excavate.** The old implementations are in git history and they are
   *known-wrong*. Resurrecting one — even as "reference" — reintroduces the
   testimony the deletion was buying back.
2. **Find the prose.** Deliberate holes are documented — in this knowledge
   base, in design records, in the architecture notes. The intended shape
   exists in words.
3. **If the prose is missing, ask.** A hole with no documentation is either
   an oversight in the docs (fixable) or not actually deliberate (also
   informative). Either way the answer is a question, not an implementation.

The complementary practice: knowledge that must survive gets written down
*deliberately* — decision records, architecture answers, this site. Curation
runs in both directions: remove what misleads, write what steers.
