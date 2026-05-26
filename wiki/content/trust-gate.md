+++
title = "Trust gate"

[extra]
type = "Glossary"
term = "Trust gate"
created = "2026-05-26"
tags = ["cache", "trust", "security"]
+++

The per-cache ingress policy: the code that decides which signatures / attestations / producer identities are acceptable for entries being inserted into that cache. Different caches have different gates. Local caches gate by "I wrote it." Public caches gate by "trusted builder signed it." Customer caches gate by whatever the customer configures.

See [[Trust is per-cache, not global]].
