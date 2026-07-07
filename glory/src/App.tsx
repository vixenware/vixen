import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Background,
  Controls,
  ReactFlow,
  type Edge,
  type Node,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import dagre from "@dagrejs/dagre";
import SkillNode from "./SkillNode";
import type { Progress, SkillNodeData } from "./types";

const nodeTypes = { skill: SkillNode };
const POLL_MS = 2000;

function layout(progress: Progress): { nodes: Node[]; edges: Edge[] } {
  const byId = new Map(progress.nodes.map((n) => [n.id, n]));
  const doneSet = new Set(
    progress.nodes.filter((n) => n.status === "done").map((n) => n.id),
  );
  const g = new dagre.graphlib.Graph();
  g.setGraph({ rankdir: "LR", ranksep: 90, nodesep: 26, marginx: 40, marginy: 40 });
  g.setDefaultEdgeLabel(() => ({}));

  for (const n of progress.nodes) {
    const goal = n.status === "goal";
    g.setNode(n.id, { width: goal ? 250 : 215, height: goal ? 86 : 64 });
  }
  const edges: Edge[] = [];
  for (const n of progress.nodes) {
    for (const dep of n.deps ?? []) {
      if (!byId.has(dep)) continue;
      g.setEdge(dep, n.id);
      const src = byId.get(dep)!;
      const lit = src.status === "done";
      edges.push({
        id: `${dep}->${n.id}`,
        source: dep,
        target: n.id,
        animated: byId.get(n.id)!.status === "active",
        style: {
          stroke: lit ? "var(--edge-lit)" : "var(--edge-dim)",
          strokeWidth: lit ? 1.8 : 1.2,
          opacity: lit ? 0.9 : 0.45,
        },
      });
    }
  }
  dagre.layout(g);

  const nodes: Node[] = progress.nodes.map((n) => {
    const pos = g.node(n.id);
    const unlocked = (n.deps ?? []).every((d) => doneSet.has(d));
    return {
      id: n.id,
      type: "skill",
      position: { x: pos.x - pos.width / 2, y: pos.y - pos.height / 2 },
      data: { ...n, laneColor: progress.lanes[n.lane] ?? "#8892a6", unlocked },
    };
  });
  return { nodes, edges };
}

export default function App() {
  const [progress, setProgress] = useState<Progress | null>(null);
  const [selected, setSelected] = useState<SkillNodeData | null>(null);
  const [pulse, setPulse] = useState(false);
  const lastText = useRef<string>("");

  useEffect(() => {
    let alive = true;
    const tick = async () => {
      try {
        const res = await fetch(`/progress.json?t=${Date.now()}`);
        const text = await res.text();
        if (!alive || text === lastText.current) return;
        lastText.current = text;
        setProgress(JSON.parse(text));
        setPulse(true);
        setTimeout(() => setPulse(false), 900);
      } catch {
        /* keep last good tree */
      }
    };
    tick();
    const h = setInterval(tick, POLL_MS);
    return () => {
      alive = false;
      clearInterval(h);
    };
  }, []);

  const graph = useMemo(
    () => (progress ? layout(progress) : { nodes: [], edges: [] }),
    [progress],
  );

  const onNodeClick = useCallback(
    (_e: unknown, node: Node) => setSelected(node.data as SkillNodeData),
    [],
  );

  if (!progress) return <div className="loading">summoning the tree…</div>;

  const counts = progress.nodes.reduce<Record<string, number>>((acc, n) => {
    acc[n.status] = (acc[n.status] ?? 0) + 1;
    return acc;
  }, {});

  return (
    <div className="app">
      <header className="hud">
        <div className="hud-title">
          <h1>{progress.meta.title}</h1>
          <span className="hud-sub">{progress.meta.subtitle}</span>
        </div>
        <div className="hud-right">
          <span className="hud-trunk">{progress.meta.trunk}</span>
          <span className={`hud-live${pulse ? " pulse" : ""}`}>
            ● live · {new Date(progress.updated).toLocaleTimeString()}
          </span>
          <span className="hud-counts">
            ✦ {counts.done ?? 0} · ◉ {counts.active ?? 0} · queued{" "}
            {counts.queued ?? 0} · ⏸ {counts.parked ?? 0}
          </span>
        </div>
      </header>
      <div className="legend">
        {Object.entries(progress.lanes).map(([lane, color]) => (
          <span key={lane} className="legend-item">
            <i style={{ background: color }} /> {lane}
          </span>
        ))}
      </div>
      <ReactFlow
        nodes={graph.nodes}
        edges={graph.edges}
        nodeTypes={nodeTypes}
        onNodeClick={onNodeClick}
        onPaneClick={() => setSelected(null)}
        fitView
        minZoom={0.15}
        proOptions={{ hideAttribution: true }}
      >
        <Background color="#1b2233" gap={28} />
        <Controls showInteractive={false} />
      </ReactFlow>
      {selected && (
        <aside className="panel">
          <div className="panel-status">{selected.status}</div>
          <h2>{selected.title}</h2>
          <p>{selected.desc}</p>
          {selected.evidence && selected.evidence.length > 0 && (
            <>
              <h3>evidence</h3>
              <ul>
                {selected.evidence.map((e) => (
                  <li key={e}>{e}</li>
                ))}
              </ul>
            </>
          )}
          {selected.deps && selected.deps.length > 0 && (
            <>
              <h3>requires</h3>
              <ul>
                {selected.deps.map((d) => (
                  <li key={d}>{d}</li>
                ))}
              </ul>
            </>
          )}
        </aside>
      )}
    </div>
  );
}
