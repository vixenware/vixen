// The demand graph, v2: events carry run ids, spans and ancestry now, so this
// renders what actually happened — an indented demand tree (depth = who
// demanded whom) with EXACT spawn→exec arcs (paired by run id, not guessed),
// cross-linked to the editor through source spans.

import type { DemandEvent, Serving, Span } from "./generated/daemon";

const ROW_H = 30;
const GUTTER = 46;
const INDENT = 22;
const GLYPH_X = GUTTER + 12;

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
    case "Created":
      return "ev-spawn";
    case "Scheduled":
      return "ev-scheduled";
    case "Finished":
      return `ev-exec ev-exec-${e.serving.tag.toLowerCase()}`;
    case "Observation":
      return e.replayed ? "ev-obs-replayed" : "ev-obs-cold";
    case "Done":
      return "ev-done";
    case "Failed":
      return "ev-failed";
    case "Summary":
      return "ev-done";
  }
}

function trunc(s: string, n: number): string {
  return s.length > n ? s.slice(0, n - 1) + "…" : s;
}

/// The label says WHAT; the glyph's shape/color says HOW (status words live
/// in the tooltip and the legend, not in the row).
function label(e: DemandEvent): string {
  switch (e.tag) {
    case "Miss":
    case "Hit": {
      const args = trunc(e.args.map(([, v]) => v).join(", "), 46);
      return `fn ${e.func}(${args})`;
    }
    case "Created":
      return `${e.command}: ${trunc(e.describe[0] ?? "", 56)}`;
    case "Scheduled":
      return `${e.command}: run ${e.run}`;
    case "Finished": {
      const names = e.outputs.map(([p]) => p.split("/").pop() ?? p).join(", ");
      return `→ ${trunc(names, 52)}`;
    }
    case "Observation":
      return trunc(e.key, 56);
    case "Done":
      return "done";
    case "Failed":
      return "failed";
    case "Summary":
      return "summary";
  }
}

/// Full status, for the row tooltip.
function statusText(e: DemandEvent): string {
  switch (e.tag) {
    case "Miss":
      return "memo miss — evaluated";
    case "Hit":
      return "memo hit — served from cache";
    case "Created":
      return "thunk created (pending tree)";
    case "Scheduled":
      return "execution scheduled — first demand touched this run";
    case "Finished":
      return servingLabel(e.serving);
    case "Observation":
      return e.replayed ? "replayed pin" : "observed the world (cold)";
    case "Done":
      return "evaluation finished";
    case "Failed":
      return "evaluation failed";
    case "Summary":
      return "evaluation summary";
  }
}

function atLabel(e: DemandEvent): string | null {
  switch (e.tag) {
    case "Done":
    case "Failed":
    case "Summary":
      return null;
    default:
      return `+${(Number(e.at) / 1000).toFixed(1)}ms`;
  }
}

function spanOf(e: DemandEvent): Span | null {
  switch (e.tag) {
    case "Miss":
    case "Hit":
    case "Created":
    case "Scheduled":
    case "Finished":
      return e.span;
    default:
      return null;
  }
}

function glyph(e: DemandEvent, x: number, y: number) {
  const cls = rowClass(e);
  switch (e.tag) {
    case "Observation":
      return (
        <rect
          className={cls}
          x={x - 5.5}
          y={y - 5.5}
          width={11}
          height={11}
          transform={`rotate(45 ${x} ${y})`}
        />
      );
    case "Created":
      return <circle className={cls} cx={x} cy={y} r={5.5} fill="none" strokeDasharray="2.5 2" />;
    case "Scheduled":
      return <rect className={cls} x={x - 5.5} y={y - 5.5} width={11} height={11} rx={2.5} fill="none" />;
    case "Finished":
      return <rect className={cls} x={x - 6} y={y - 6} width={12} height={12} rx={2.5} />;
    default:
      return <circle className={cls} cx={x} cy={y} r={5.5} />;
  }
}

interface Row {
  event: DemandEvent;
  depth: number;
}

/// Reconstruct the demand tree from the event stream. Events arrive in demand
/// order; `caller`/`in_fn` name the demanding fn, and the last-seen depth of
/// that fn is the parent level. (Instances of the same fn reuse the latest
/// depth — exact per-instance ancestry wants run-record parents, later.)
function layout(events: DemandEvent[]): Row[] {
  const fnDepth = new Map<string, number>();
  const runDepth = new Map<bigint, number>();
  let lastFnDepth = -1;
  return events.map((e) => {
    let depth = 0;
    switch (e.tag) {
      case "Miss":
      case "Hit": {
        const parent = e.caller !== null ? fnDepth.get(e.caller) : undefined;
        depth = parent !== undefined ? parent + 1 : 0;
        if (e.tag === "Miss") {
          fnDepth.set(e.func, depth);
          lastFnDepth = depth;
        }
        break;
      }
      case "Created": {
        const parent = e.in_fn !== null ? fnDepth.get(e.in_fn) : undefined;
        depth = parent !== undefined ? parent + 1 : 0;
        runDepth.set(e.run, depth);
        break;
      }
      case "Scheduled":
      case "Finished":
        depth = runDepth.get(e.run) ?? 0;
        break;
      case "Observation":
        depth = lastFnDepth + 1;
        break;
      case "Done":
      case "Failed":
      case "Summary":
        depth = 0;
        break;
    }
    return { event: e, depth };
  });
}

export function DemandGraph(props: {
  events: DemandEvent[];
  settled: boolean;
  showAll: boolean;
  cursor: number | null;
  onSelect: (event: DemandEvent) => void;
}) {
  const { events, settled, showAll, cursor, onSelect } = props;
  // Work-only by default: hide what we did NOT pay for — memo hits,
  // replayed pins, and thunks that were never even scheduled. "all" shows
  // the full demand story including the laziness wins.
  const scheduledRuns = new Set(
    events.filter((e) => e.tag === "Scheduled").map((e) => (e.tag === "Scheduled" ? e.run : 0n)),
  );
  const paid = (e: DemandEvent): boolean => {
    switch (e.tag) {
      case "Hit":
        return false;
      case "Observation":
        return !e.replayed;
      case "Created":
        return scheduledRuns.has(e.run);
      default:
        return true;
    }
  };
  const rows = layout(events).filter((r) => showAll || paid(r.event));
  const height = rows.length * ROW_H + 12;

  // Exact spawn↔exec pairing: the run id IS the edge.
  const spawnRowOfRun = new Map<bigint, number>();
  const arcs: Array<{ from: number; to: number }> = [];
  rows.forEach(({ event: e }, i) => {
    if (e.tag === "Created") spawnRowOfRun.set(e.run, i);
    else if (e.tag === "Scheduled" || e.tag === "Finished") {
      const from = spawnRowOfRun.get(e.run);
      if (from !== undefined) arcs.push({ from, to: i });
    }
  });

  const rowY = (i: number) => i * ROW_H + ROW_H / 2 + 6;
  const glyphX = (depth: number) => GLYPH_X + depth * INDENT;

  return (
    <div>
      <div className="legend">
        <span className="lg lg-miss">● miss</span>
        <span className="lg lg-hit">● hit</span>
        <span className="lg lg-spawn">◌ created</span>
        <span className="lg lg-scheduled">□ scheduled</span>
        <span className="lg lg-ran">■ ran</span>
        <span className="lg lg-t1">■ tier-1</span>
        <span className="lg lg-t2">■ tier-2</span>
        <span className="lg lg-joined">■ joined</span>
        <span className="lg lg-obs">◇ observed</span>
        <span className="lg lg-replay">◆ replayed</span>
      </div>
      <svg className="demand-graph" width="100%" height={height}>
      {arcs.map((a, i) => {
        const y1 = rowY(a.from);
        const y2 = rowY(a.to);
        const x = glyphX(rows[a.from].depth);
        const bulge = Math.min(GUTTER - 6, 14 + (y2 - y1) / 10);
        return (
          <path
            key={i}
            className="arc"
            d={`M ${x - 8} ${y1} C ${x - bulge} ${y1}, ${x - bulge} ${y2}, ${x - 8} ${y2}`}
            fill="none"
          />
        );
      })}
      {rows.map(({ event: e, depth }, i) => {
        const y = rowY(i);
        const x = glyphX(depth);
        const span = spanOf(e);
        const cursorHit =
          span !== null && cursor !== null && cursor >= span.start && cursor < span.end;
        const ghost = settled && e.tag === "Created" && !scheduledRuns.has(e.run);
        return (
          <g
            key={i}
            className={`row${span ? " row-linked" : ""}${cursorHit ? " row-cursor" : ""}${ghost ? " row-ghost" : ""}`}
            onClick={() => onSelect(e)}
          >
            {cursorHit && (
              <rect className="cursor-hit" x={0} y={y - ROW_H / 2 + 2} width="100%" height={ROW_H - 4} rx={5} />
            )}
            {glyph(e, x, y)}
            <text className={`lbl ${rowClass(e)}`} x={x + 18} y={y + 4}>
              {label(e)}
            </text>
            {atLabel(e) !== null && (
              <text className="at" x="100%" y={y + 4} textAnchor="end" dx={-8}>
                {atLabel(e)}
              </text>
            )}
            <title>{ghost ? "thunk created — never demanded (you did not pay for this)" : statusText(e)}</title>
          </g>
        );
      })}
      </svg>
    </div>
  );
}
