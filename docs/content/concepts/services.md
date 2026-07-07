+++
title = "Daemons, runners, and services"
+++

vixen runs as a small set of cooperating services, and the division of labor is
worth knowing even as a user, because it explains what runs where — on your
laptop, on a build box, in CI.

**The daemon** runs on every participating machine, whether or not that machine
ever builds anything. It provides *ambient capabilities* as services:

- mounting resources from the content store onto virtual filesystem paths,
- launching processes under the sandbox,
- watching local disk for changes.

A machine with only a daemon is still useful — it can host stores, serve
mounts, observe a working copy.

**The runner** is the service that *advertises build capability*: which
toolchains are present, what work it will accept. Work is dispatched *to* a
runner; the runner then composes the daemon's capabilities — "mount these
inputs, launch this compiler under the sandbox, flush these outputs" — over
RPC. The runner doesn't own filesystem or sandbox machinery; it orchestrates
the daemon's.

**The CLI** talks to a daemon; the daemon dispatches to a runner. On a single
machine this whole conversation happens locally and invisibly — but it is the
*same conversation* that happens when the runner is on another machine. Remote
build isn't a different architecture; it's the same architecture with longer
wires. Content addressing does the rest: values are identified by hash, so it
never matters *where* one was computed.

**The protocol is the boundary.** The shapes of these conversations — mount,
run, flush, the observed read-set that comes back — are typed RPC
([vox](@/constellation/vox.md)) and are part of vixen's public surface. The
implementations behind them (the sandbox profiles, the VFS internals) are the
private half. If you're integrating with vixen, you target the protocol; if
you're extending vixen, you implement capabilities behind it.
