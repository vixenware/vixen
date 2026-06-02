#!/usr/bin/env python3
"""One-shot: Facet-derive the VIR node universe.

For each node file: add `facet::Facet` to every `#[derive(...)]` that lacks it,
and insert `#[repr(u8)]` before every `pub enum` (Facet enums need an explicit
repr). Doc comments sit above the derive, and the derive sits immediately above
the type decl, so peeking one line ahead is sufficient.
"""

import re

BASE = "crates/vx-ir/src/"
FILES = [
    "expr.rs", "stmt.rs", "pattern.rs", "binding.rs", "body.rs",
    "item.rs", "scope.rs", "type_ref.rs", "def.rs", "source_unit.rs", "arena.rs",
]

derive_re = re.compile(r'^(\s*)#\[derive\((.*)\)\]\s*$')
enum_re = re.compile(r'^\s*pub(\(crate\))? enum ')

for fn in FILES:
    path = BASE + fn
    with open(path) as f:
        lines = f.readlines()
    out = []
    derives = reprs = 0
    i, n = 0, len(lines)
    while i < n:
        line = lines[i]
        m = derive_re.match(line)
        if m:
            indent, inner = m.group(1), m.group(2)
            if 'Facet' not in inner:
                inner = 'facet::Facet, ' + inner
                derives += 1
            out.append(f'{indent}#[derive({inner})]\n')
            if i + 1 < n and enum_re.match(lines[i + 1]):
                out.append(f'{indent}#[repr(u8)]\n')
                reprs += 1
            i += 1
            continue
        out.append(line)
        i += 1
    with open(path, 'w') as f:
        f.writelines(out)
    print(f"{fn}: +{derives} Facet, +{reprs} repr(u8)")
