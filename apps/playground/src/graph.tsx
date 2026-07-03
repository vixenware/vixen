// The demand graph, v1: a vertical rail of demand events in the order the
// evaluator demanded them, with arcs joining each Spawn to the Exec that
// resolved it. Real ancestry edges (which demand caused which) need
// input/output tree hashes on the oracle's events — next slice, daemon side.

import type { DemandEvent, Serving } from "./generated/daemon";

const ROW_H = 30;
const GUTTER = 46;
const GLYPH_X = GUTTER + 12;
const LABEL_X = GLYPH_X + 18;

function servingLabel(s: Serving): string {
  switch (s.tag) {
    case "Ran":
      return "ran";
    case "Tier1Hit":
      return "tier-1 hit";
    case "Tier2Cutoff":
      return `tier-2 cutoff (${s.verified} verified)`;
    case "Joined":
      return "joined in-flight";
  }
}

function rowClass(e: DemandEvent): string {
  switch (e.tag) {
    case "Miss":
      return "ev-miss";
    case "Hit":
      return "ev-hit";
    case "Spawn":
      return "ev-spawn";
    case "Exec":
      return `ev-exec ev-exec-${e.serving.tag.toLowerCase()}`;
    case "Observation":
      return e.replayed ? "ev-obs-replayed" : "ev-obs-cold";
    case "Done":
      return "ev-done";
    case "Failed":
      return "ev-failed";
  }
}

function label(e: DemandEvent): string {
  switch (e.tag) {
    case "Miss":
      return `fn ${e.func} — miss, evaluating`;
    case "Hit":
      return `fn ${e.func} — memo hit`;
    case "Spawn":
      return `spawn ${e.command} (pending tree)`;
    case "Exec":
      return `exec ${e.command} — ${servingLabel(e.serving)}`;
    case "Observation":
      return `${e.key} — ${e.replayed ? "replayed pin" : "observed (cold)"}`;
    case "Done":
      return "done";
    case "Failed":
      return "failed";
  }
}

function glyph(e: DemandEvent, y: number) {
  const cls = rowClass(e);
  switch (e.tag) {
    case "Observation":
      // A diamond: the boundary with the outside world.
      return (
        <rect
          className={cls}
          x={GLYPH_X - 5.5}
          y={y - 5.5}
          width={11}
          height={11}
          transform={`rotate(45 ${GLYPH_X} ${y})`}
        />
      );
    case "Spawn":
      return <circle className={cls} cx={GLYPH_X} cy={y} r={5.5} fill="none" strokeDasharray="2.5 2" />;
    case "Exec":
      return <rect className={cls} x={GLYPH_X - 6} y={y - 6} width={12} height={12} rx={2.5} />;
    default:
      return <circle className={cls} cx={GLYPH_X} cy={y} r={5.5} />;
  }
}

export function DemandGraph(props: { events: DemandEvent[]; settled: boolean }) {
  const { events, settled } = props;
  const height = events.length * ROW_H + 12;

  // Pair each Spawn with the Exec that resolved it (FIFO per command name).
  const arcs: Array<{ from: number; to: number }> = [];
  const open = new Map<string, number[]>();
  events.forEach((e, i) => {
    if (e.tag === "Spawn") {
      const q = open.get(e.command) ?? [];
      q.push(i);
      open.set(e.command, q);
    } else if (e.tag === "Exec") {
      const q = open.get(e.command);
      const from = q?.shift();
      if (from !== undefined) arcs.push({ from, to: i });
    }
  });
  // Once the evaluation settles, a spawn with no Exec is the language being
  // maximally lazy: its outputs were only ever PROJECTED (single paths pulled
  // from the live run), so nothing forced the flush. Projection doesn't force.
  const neverFlushed = new Set<number>();
  if (settled) {
    for (const q of open.values()) for (const i of q) neverFlushed.add(i);
  }

  const rowY = (i: number) => i * ROW_H + ROW_H / 2 + 6;

  return (
    <svg className="demand-graph" width="100%" height={height}>
      {/* The demand rail. */}
      <line
        className="rail"
        x1={GLYPH_X}
        y1={rowY(0)}
        x2={GLYPH_X}
        y2={rowY(events.length - 1)}
      />
      {/* Spawn → Exec arcs: the life of a pending tree, from dispatch to
          whatever the exec cache served. */}
      {arcs.map((a, i) => {
        const y1 = rowY(a.from);
        const y2 = rowY(a.to);
        const bulge = Math.min(GUTTER - 6, 14 + (y2 - y1) / 10);
        return (
          <path
            key={i}
            className="arc"
            d={`M ${GLYPH_X - 8} ${y1} C ${GLYPH_X - bulge} ${y1}, ${GLYPH_X - bulge} ${y2}, ${GLYPH_X - 8} ${y2}`}
            fill="none"
          />
        );
      })}
      {events.map((e, i) => {
        const y = rowY(i);
        return (
          <g key={i} className="row">
            {glyph(e, y)}
            <text className={`lbl ${rowClass(e)}`} x={LABEL_X} y={y + 4}>
              {label(e)}
              {neverFlushed.has(i) && (
                <tspan className="lazy-note"> — projected only, never flushed</tspan>
              )}
            </text>
          </g>
        );
      })}
    </svg>
  );
}
