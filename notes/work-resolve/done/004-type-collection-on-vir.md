# 004: Type Collection on VIR

Status: Not started.

## Goal

Migrate type collection (type_collect.rs, 868 lines) from Rowan AST to VIR.
Two-pass algorithm: register type declarations, then fill in field types
and signatures.

## Depends On

000, 001.
