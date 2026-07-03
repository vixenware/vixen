// Code intelligence via snark-wasm: vix's embedded grammar + highlights query
// + binder, compiled to wasm. Two calls, no grammar assets client-side:
//   vixHighlights(source) — syntax captures (byte ranges)
//   vixBindings(source)   — symbols/references (occurrences, go-to-def)
// Results are cached per source string; the wasm parser reuses its tables.

import init, { vixBindings, vixHighlights } from "@bearcove/snark-wasm";

export interface Capture {
  capture: string;
  start: number;
  end: number;
}

export interface IdeSymbol {
  name: string;
  kind: string;
  start: number;
  end: number;
}

export interface Ref {
  start: number;
  end: number;
  symbol: number;
}

export interface Bindings {
  error: string | null;
  symbols: IdeSymbol[];
  refs: Ref[];
  unresolved: { name: string; start: number; end: number }[];
}

let ready = false;
const initPromise = init().then(() => {
  ready = true;
});

/// Resolves once the wasm module is loaded; callers refresh their view then.
export function whenReady(): Promise<void> {
  return initPromise;
}

let hlSource = "";
let hlResult: Capture[] = [];

export function highlightsFor(source: string): Capture[] {
  if (!ready) return [];
  if (source !== hlSource) {
    hlSource = source;
    const parsed = JSON.parse(vixHighlights(source)) as {
      error: string | null;
      highlights: Capture[];
    };
    // On a parse error, keep the previous decorations rather than flashing
    // to monochrome mid-edit.
    if (parsed.error === null) hlResult = parsed.highlights;
  }
  return hlResult;
}

let bSource = "";
let bResult: Bindings | null = null;

export function bindingsFor(source: string): Bindings | null {
  if (!ready) return null;
  if (source !== bSource) {
    bSource = source;
    const parsed = JSON.parse(vixBindings(source)) as Bindings;
    bResult = parsed.error === null ? parsed : bResult;
  }
  return bResult;
}

/// All occurrence spans (definition + references) of the symbol under the
/// given BYTE offset, or null when the offset isn't on a symbol.
export function occurrencesAt(
  source: string,
  byte: number,
): { start: number; end: number }[] | null {
  const b = bindingsFor(source);
  if (b === null) return null;
  let symbol: number | null = null;
  for (let i = 0; i < b.symbols.length; i++) {
    const s = b.symbols[i];
    if (byte >= s.start && byte < s.end) {
      symbol = i;
      break;
    }
  }
  if (symbol === null) {
    for (const r of b.refs) {
      if (byte >= r.start && byte < r.end) {
        symbol = r.symbol;
        break;
      }
    }
  }
  if (symbol === null) return null;
  const def = b.symbols[symbol];
  const spans = [{ start: def.start, end: def.end }];
  for (const r of b.refs) {
    if (r.symbol === symbol) spans.push({ start: r.start, end: r.end });
  }
  spans.sort((a, z) => a.start - z.start);
  return spans;
}
