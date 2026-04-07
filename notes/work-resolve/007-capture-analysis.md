# 007: Capture Analysis

Status: Not started.

## Goal

For each nested body (closure/thunk), compute free variables by walking
its expression tree and finding references to bindings owned by ancestor
bodies. Produces explicit capture sets.

Replaces the current "capture all parent bindings then prune after lowering"
approach in exec_slice.rs.

## Output

Per body: Vec<BindingId> of captured bindings from parent scopes.

## Depends On

003 (name resolution — need to know what each NameRef resolves to).
