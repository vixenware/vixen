import { useMemo, useState } from "react";
import {
  Background,
  BackgroundVariant,
  Controls,
  MiniMap,
  ReactFlow,
  type Edge,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";

import { SkillNode } from "./SkillNode";
import { parseTree, toFlow, type SkillFlowNode, type SkillSpec, type Status } from "./tree";
import treeSource from "./tree.yaml?raw";

const nodeTypes = { skill: SkillNode };

const STATUS_LABEL: Record<Status, string> = {
  done: "Done",
  in_progress: "In progress",
  planned: "Planned",
  idea: "Idea",
};

export default function App() {
  const parsed = useMemo(() => {
    try {
      const tree = parseTree(treeSource);
      return { tree, flow: toFlow(tree), error: null as string | null };
    } catch (e) {
      return { tree: null, flow: null, error: e instanceof Error ? e.message : String(e) };
    }
  }, []);

  const [selected, setSelected] = useState<SkillSpec | null>(null);

  if (parsed.error || !parsed.tree || !parsed.flow) {
    return (
      <div className="error-screen">
        <h1>Skill tree failed to load</h1>
        <pre>{parsed.error}</pre>
      </div>
    );
  }

  const { tree, flow } = parsed;

  return (
    <div className="app">
      <header className="topbar">
        <div>
          <h1>{tree.meta?.title ?? "Vixen — Skill Tree"}</h1>
          {tree.meta?.subtitle && <p>{tree.meta.subtitle}</p>}
        </div>
        <Legend categories={tree.categories} />
      </header>

      <div className="canvas">
        <ReactFlow<SkillFlowNode, Edge>
          nodes={flow.nodes}
          edges={flow.edges}
          nodeTypes={nodeTypes}
          fitView
          minZoom={0.2}
          maxZoom={1.75}
          proOptions={{ hideAttribution: true }}
          onNodeClick={(_, node) => setSelected(node.data.spec)}
          onPaneClick={() => setSelected(null)}
        >
          <Background variant={BackgroundVariant.Dots} gap={28} size={1} color="#2a2f3a" />
          <MiniMap
            pannable
            zoomable
            bgColor="#141925"
            maskColor="rgba(13, 16, 23, 0.7)"
            nodeColor={(n) => (n.data as SkillFlowNode["data"]).category?.color ?? "#7a8190"}
            nodeStrokeWidth={0}
          />
          <Controls showInteractive={false} />
        </ReactFlow>

        {selected && (
          <aside className="detail">
            <button className="close" onClick={() => setSelected(null)} aria-label="Close">
              ×
            </button>
            <span className={`badge status-${selected.status}`}>{STATUS_LABEL[selected.status]}</span>
            <h2>{selected.title}</h2>
            {selected.category && tree.categories[selected.category] && (
              <span className="detail-cat" style={{ color: tree.categories[selected.category].color }}>
                {tree.categories[selected.category].label ?? selected.category}
              </span>
            )}
            {selected.hands && <p className="hands-note">✋ Good candidate for extra hands (Hexcat)</p>}
            {selected.summary && <p className="lead">{selected.summary}</p>}
            {selected.detail && <p>{selected.detail}</p>}
            {selected.payoff && (
              <div className="payoff">
                <h3>Why it matters</h3>
                <p>{selected.payoff}</p>
              </div>
            )}
            {selected.requires && selected.requires.length > 0 && (
              <div className="requires">
                <h3>Requires</h3>
                <ul>
                  {selected.requires.map((id) => {
                    const dep = tree.nodes.find((n) => n.id === id);
                    return <li key={id}>{dep?.title ?? id}</li>;
                  })}
                </ul>
              </div>
            )}
          </aside>
        )}
      </div>
    </div>
  );
}

function Legend({ categories }: { categories: Record<string, { label?: string; color: string }> }) {
  const entries = Object.entries(categories);
  if (entries.length === 0) return null;
  return (
    <div className="legend">
      {entries.map(([key, c]) => (
        <span key={key} className="legend-item">
          <span className="swatch" style={{ background: c.color }} />
          {c.label ?? key}
        </span>
      ))}
    </div>
  );
}
