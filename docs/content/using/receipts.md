+++
title = "Receipts"
weight = 5
+++

Every build system answers "what happened?" with logs. vixen answers with a
**trace**: a structured record of every demand, every memo hit, every
computation spawned, every value stored — the machine's actual decision
sequence, not a narration of it.

We call reading this trace *reading the receipts*, because that's what it's
for: when something rebuilt, the trace shows the demand chain that pulled it
in; when something *didn't* rebuild, the trace shows the memo hit and the
content identity that justified it. "Why did this rebuild?" — historically a
forum question with folklore answers — becomes a lookup.

The events you'll meet:

- **Demanded** — a question was asked: this function, these (content-addressed)
  arguments.
- **MemoHit** — the question had already been answered; here's the existing
  answer. The workhorse event: a warm build is almost entirely these.
- **Spawned** — no memo entry; real computation started. Every Spawned is a
  cost; the game is making them rare and justified.
- **StoreAlloc** — a value froze into the content store (with a flag for "this
  content already existed" — deduplication in action).
- **Projection hits** — the fine-grained variant: this computation depended on
  *part* of a value, the part is unchanged, so the memo holds even though the
  whole value changed. This is item-granularity paying off, visibly.

Two properties make the trace more than a debugging aid. It is **complete** —
the machine cannot compute without emitting; there is no silent path. And it is
**semantic** — events reference content identities, so two traces can be
compared: same question, same answer, same *reasoning*. vixen's own test suite
asserts on trace shapes (that laziness held, that nothing spawned twice); your
CI can make the same kinds of assertions about your builds — "this change class
should never rebuild that crate" as a checkable statement.

The interactive form — expand a build, follow a demand chain, diff two runs —
is a UI milestone tracked on [Status](/status/). The trace itself
exists today; it's how the machine is tested.
