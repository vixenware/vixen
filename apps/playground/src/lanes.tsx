// Lanes view: profiler-style left-to-right timeline per command kind.
// One lane per command (cc, ar, rustc, …). Fleet executor lanes come later —
// the backend doesn't emit them yet.
//
// Each run with a Scheduled event → rectangle from Scheduled.at to Finished.at.
// Each run with only a Created event → small dashed tick at Created.at.
// Open-ended scheduled runs (never Finished) → hatch ghost bar to timeline end.

import { useEffect, useRef, useState } from "react";
import type { DemandEvent, Serving } from "./generated/daemon";

const GUTTER_W = 90;
const LANE_H = 32;
const AXIS_H = 28;
const LANE_PAD = 6;
const MIN_RECT_W = 3;
const MIN_TIMELINE_MS_PX = 2;
const TICK_COUNT = 6;

type CreatedEvent = Extract<DemandEvent, { tag: "Created" }>;
type ScheduledEvent = Extract<DemandEvent, { tag: "Scheduled" }>;
type FinishedEvent = Extract<DemandEvent, { tag: "Finished" }>;

function servingCssClass(s: Serving): string {
  return `ev-exec-${s.tag.toLowerCase()}`;
}

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

export function Lanes(props: {
  events: DemandEvent[];
  onSelect: (event: DemandEvent) => void;
}) {
  const { events, onSelect } = props;
  const containerRef = useRef<HTMLDivElement>(null);
  const [containerW, setContainerW] = useState(600);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const obs = new ResizeObserver(([entry]) => {
      setContainerW(entry.contentRect.width);
    });
    obs.observe(el);
    return () => obs.disconnect();
  }, []);

  const createdByRun = new Map<bigint, CreatedEvent>();
  const scheduledByRun = new Map<bigint, ScheduledEvent>();
  const finishedByRun = new Map<bigint, FinishedEvent>();

  for (const e of events) {
    if (e.tag === "Created") createdByRun.set(e.run, e);
    else if (e.tag === "Scheduled") scheduledByRun.set(e.run, e);
    else if (e.tag === "Finished") finishedByRun.set(e.run, e);
  }

  const commandSet = new Set<string>();
  for (const e of events) {
    if (e.tag === "Created" || e.tag === "Scheduled" || e.tag === "Finished") {
      commandSet.add(e.command);
    }
  }

  if (commandSet.size === 0) {
    return (
      <div ref={containerRef}>
        <p className="hint">No run events yet — demand an entry point to populate lanes.</p>
      </div>
    );
  }

  const lanes = [...commandSet].sort();

  // Max timestamp across all timed events (all variants except Done/Failed have `at`)
  let maxAt = 1n;
  for (const e of events) {
    if (e.tag !== "Done" && e.tag !== "Failed") {
      if (e.at > maxAt) maxAt = e.at;
    }
  }

  const maxAtMs = Number(maxAt) / 1000;
  const timelineW = Math.max(containerW - GUTTER_W, maxAtMs * MIN_TIMELINE_MS_PX);
  const svgW = GUTTER_W + timelineW;
  const svgH = AXIS_H + lanes.length * LANE_H + 4;

  const toX = (t: bigint): number =>
    GUTTER_W + (Number(t) / Number(maxAt)) * timelineW;
  const laneTop = (i: number): number => AXIS_H + i * LANE_H;

  const axisTicksMs = Array.from({ length: TICK_COUNT + 1 }, (_, i) =>
    (i / TICK_COUNT) * maxAtMs,
  );

  const barItems: React.ReactElement[] = [];
  const tickItems: React.ReactElement[] = [];

  for (const [runId, sched] of scheduledByRun) {
    const laneIdx = lanes.indexOf(sched.command);
    if (laneIdx === -1) continue;

    const created = createdByRun.get(runId);
    const finished = finishedByRun.get(runId);

    const y0 = laneTop(laneIdx) + LANE_PAD;
    const barH = LANE_H - LANE_PAD * 2;
    const x0 = toX(sched.at);
    const rawX1 = finished ? toX(finished.at) : GUTTER_W + timelineW;
    const x1 = Math.max(x0 + MIN_RECT_W, rawX1);
    const w = x1 - x0;

    const durationMs = finished
      ? ((Number(finished.at) - Number(sched.at)) / 1000).toFixed(2)
      : null;
    const describe0 = created?.describe[0] ?? sched.command;
    const tooltipText = [
      describe0,
      finished ? servingLabel(finished.serving) : "scheduled — not yet finished",
      durationMs !== null ? `${durationMs} ms` : "open-ended",
    ].join("\n");

    const clickTarget = created ?? sched;
    const barClass = finished
      ? `lane-bar ${servingCssClass(finished.serving)}`
      : "lane-bar lane-bar-ghost";

    barItems.push(
      <rect
        key={`bar-${runId}`}
        x={x0}
        y={y0}
        width={w}
        height={barH}
        rx={2}
        className={barClass}
        fill={finished ? undefined : "url(#lane-hatch)"}
        style={{ cursor: "pointer" }}
        onClick={() => onSelect(clickTarget)}
      >
        <title>{tooltipText}</title>
      </rect>,
    );
  }

  for (const [runId, created] of createdByRun) {
    if (scheduledByRun.has(runId)) continue;
    const laneIdx = lanes.indexOf(created.command);
    if (laneIdx === -1) continue;

    const x = toX(created.at);
    const y0 = laneTop(laneIdx) + LANE_PAD;
    const barH = LANE_H - LANE_PAD * 2;
    const describe0 = created.describe[0] ?? created.command;

    tickItems.push(
      <line
        key={`tick-${runId}`}
        className="lane-tick"
        x1={x}
        y1={y0}
        x2={x}
        y2={y0 + barH}
        style={{ cursor: "pointer" }}
        onClick={() => onSelect(created)}
      >
        <title>{`${describe0}\ncreated — not scheduled`}</title>
      </line>,
    );
  }

  return (
    <div ref={containerRef} className="lanes-view">
      <svg
        className="lanes-svg"
        width={svgW}
        height={svgH}
        style={{ display: "block", minWidth: svgW }}
      >
        <defs>
          <pattern
            id="lane-hatch"
            width={6}
            height={6}
            patternUnits="userSpaceOnUse"
            patternTransform="rotate(45)"
          >
            <line
              x1={0}
              y1={0}
              x2={0}
              y2={6}
              stroke="var(--info)"
              strokeWidth={1.5}
              strokeOpacity={0.35}
            />
          </pattern>
        </defs>

        {/* Lane backgrounds and labels */}
        {lanes.map((cmd, i) => {
          const y = laneTop(i);
          return (
            <g key={cmd}>
              <rect
                x={0}
                y={y}
                width={svgW}
                height={LANE_H}
                className={i % 2 === 0 ? "lane-row-even" : "lane-row-odd"}
              />
              <text
                x={GUTTER_W - 8}
                y={y + LANE_H / 2 + 4}
                textAnchor="end"
                className="lane-label"
              >
                {cmd}
              </text>
              <line
                x1={GUTTER_W}
                y1={y + LANE_H}
                x2={svgW}
                y2={y + LANE_H}
                className="lane-sep"
              />
            </g>
          );
        })}

        {/* Gutter right border */}
        <line
          x1={GUTTER_W}
          y1={AXIS_H}
          x2={GUTTER_W}
          y2={svgH}
          className="lane-gutter-sep"
        />

        {/* Time axis */}
        <line
          x1={GUTTER_W}
          y1={AXIS_H - 1}
          x2={GUTTER_W + timelineW}
          y2={AXIS_H - 1}
          className="lane-axis"
        />
        {axisTicksMs.map((tMs, i) => {
          const x = GUTTER_W + (tMs / maxAtMs) * timelineW;
          const anchor =
            i === 0 ? "start" : i === TICK_COUNT ? "end" : "middle";
          return (
            <g key={i}>
              <line
                x1={x}
                y1={AXIS_H - 6}
                x2={x}
                y2={AXIS_H - 1}
                className="lane-tick-mark"
              />
              <text
                x={x}
                y={AXIS_H - 9}
                textAnchor={anchor}
                className="lane-axis-label"
              >
                {tMs === 0 ? "0" : `${tMs < 10 ? tMs.toFixed(2) : tMs.toFixed(1)}ms`}
              </text>
            </g>
          );
        })}

        {barItems}
        {tickItems}
      </svg>
    </div>
  );
}
