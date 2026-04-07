# 006: Capability Checking on VIR

Status: Not started.

## Goal

Migrate capability checking (cap_check.rs, 723 lines) to VIR.
Three passes: collect fn caps, check callbacks, validate tags.

## Depends On

005 (type checking).
