+++
title = "CCC"

[extra]
type = "Glossary"
term = "CCC (Cargo Compile Cache)"
created = "2026-05-26"
tags = ["tier-1", "product"]
+++

The Tier 1 Vixen product. A forked cargo (or wrapper-binary) that intercepts the rustc shell-out and consults a remote cache before invoking. Clients are read-only against the cache; entries are produced by trusted builders fed by anonymous demand signals from clients.

See [[Tier 1 — CCC]].
