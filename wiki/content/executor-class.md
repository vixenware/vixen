+++
title = "Executor class"

[extra]
type = "Glossary"
term = "Executor class"
created = "2026-05-26"
tags = ["tier-2", "remote-exec"]
+++

A type identifying which executors are interchangeable for a given workload — for example `linux-x86_64`, `darwin-arm64`, or exotic targets like `freebsd-aarch64`. Build closures are typed against an executor class; the runtime selects an actual executor from the class at evaluation time, and can fan out across all members of a class for multi-platform builds.
