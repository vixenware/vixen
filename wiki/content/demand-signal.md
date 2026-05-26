+++
title = "Demand signal"

[extra]
type = "Glossary"
term = "Demand signal"
created = "2026-05-26"
tags = ["cache", "trust"]
+++

The unprivileged message a client emits on a cache miss: "someone asked for key K and missed." Used by the trusted-builder fleet to prioritise which cache entries to materialise. Carries no source code, no customer identity in the basic flow — just the key.

See [[Demand-warmed cache]].
