import { useMemo } from "react";
import { Background, BackgroundVariant, Controls, ReactFlow, type Edge } from "@xyflow/react";
import "@xyflow/react/dist/style.css";

import { SkillNode } from "./SkillNode";
import { parseRoadmap, toFlow, type WorkFlowNode } from "./tree";
import roadmapSource from "./tree.yaml?raw";

const nodeTypes = { work: SkillNode };

export default function App() {
  const parsed = useMemo(() => {
    try {
      const roadmap = parseRoadmap(roadmapSource);
      return { roadmap, flow: toFlow(roadmap), error: null as string | null };
    } catch (e) {
      return { roadmap: null, flow: null, error: e instanceof Error ? e.message : String(e) };
    }
  }, []);

  if (parsed.error || !parsed.flow) {
    return (
      <div className="error-screen">
        <h1>Roadmap failed to load</h1>
        <pre>{parsed.error}</pre>
      </div>
    );
  }

  return (
    <div className="app">
      <ReactFlow<WorkFlowNode, Edge>
        nodes={parsed.flow.nodes}
        edges={parsed.flow.edges}
        nodeTypes={nodeTypes}
        fitView
        minZoom={0.1}
        maxZoom={2}
        proOptions={{ hideAttribution: true }}
      >
        <Background variant={BackgroundVariant.Dots} gap={28} size={1} color="#2a2f3a" />
        <Controls showInteractive={false} />
      </ReactFlow>
    </div>
  );
}
