+++
title = "vox — typed RPC"
weight = 8
+++

vox is the RPC framework the constellation's services speak: define a service
as Rust traits over `Facet` types, and vox provides the transport (including
WebSocket), the session model, and — the part that changes how you build
tooling — **generated clients in other languages**. The TypeScript bindings are
how vixen's web UIs talk to daemons; the Swift bindings are how native apps do.

What distinguishes vox from a codegen-from-IDL system is that there is no IDL:
the Rust types *are* the schema, via [facet](/constellation/facet/), and the
wire format is [phon](/constellation/phon/) — so schema identity,
evolution-checking, and cross-language agreement are inherited rather than
reimplemented. A service's contract is content-addressed like everything else.

Streaming is first-class: channels (in both directions) are part of the type
vocabulary, which matters for a build system, whose most interesting RPCs are
long-lived — "run this and stream me the trace," "watch this directory,"
"attach to this build's progress."

In vixen's architecture, vox is the boundary line: the
[daemon/runner protocol](/concepts/services/) — mount, run, flush, observe —
is a vox service surface. Public protocol, private implementations. Anything
that wants to integrate with vixen (editors, CI glue, dashboards) talks vox to
a daemon rather than linking anything.
