# 011: Swap vx-rt to VIR Pipeline

Status: Not started.

## Goal

Make vx-rt consume from the new VIR-based pipeline in vx-resolve instead
of the old vx-analysis bridge pipeline.

Verify all existing runtime tests pass against the new pipeline.

## Depends On

010 (lowering from VIR).
