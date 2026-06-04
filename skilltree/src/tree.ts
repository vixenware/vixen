import yaml from "js-yaml";
import Dagre from "@dagrejs/dagre";
import { Position, type Edge, type Node } from "@xyflow/react";

// ---------------------------------------------------------------------------
// Data model — a roadmap. Each node is a unit of work: a title and a short
// description. `after` lists the work that must come before it (the sequencing).
// ---------------------------------------------------------------------------

export interface WorkSpec {
  id: string;
  title: string;
  description?: string;
  /** ids of work that must come first (drives the arrows / sequencing) */
  after?: string[];
}

export interface Roadmap {
  title?: string;
  nodes: WorkSpec[];
}

export interface WorkNodeData extends Record<string, unknown> {
  spec: WorkSpec;
}

export type WorkFlowNode = Node<WorkNodeData, "work">;

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

export function parseRoadmap(source: string): Roadmap {
  const raw = yaml.load(source) as Partial<Roadmap> | undefined;
  if (!raw || typeof raw !== "object") {
    throw new Error("tree.yaml did not parse to an object");
  }
  if (!Array.isArray(raw.nodes)) {
    throw new Error("tree.yaml is missing a top-level `nodes:` list");
  }

  const ids = new Set<string>();
  for (const n of raw.nodes) {
    if (!n.id) throw new Error(`a node is missing an \`id\`: ${JSON.stringify(n)}`);
    if (ids.has(n.id)) throw new Error(`duplicate node id: ${n.id}`);
    ids.add(n.id);
  }
  for (const n of raw.nodes) {
    for (const dep of n.after ?? []) {
      if (!ids.has(dep)) throw new Error(`node "${n.id}" comes after unknown node "${dep}"`);
    }
  }

  return { title: raw.title, nodes: raw.nodes };
}

// ---------------------------------------------------------------------------
// React Flow conversion + dagre layout
// ---------------------------------------------------------------------------

const NODE_W = 300;

export function toFlow(roadmap: Roadmap): { nodes: WorkFlowNode[]; edges: Edge[] } {
  const nodes: WorkFlowNode[] = roadmap.nodes.map((spec) => ({
    id: spec.id,
    type: "work",
    position: { x: 0, y: 0 },
    data: { spec },
  }));

  const edges: Edge[] = [];
  for (const spec of roadmap.nodes) {
    for (const dep of spec.after ?? []) {
      edges.push({ id: `${dep}->${spec.id}`, source: dep, target: spec.id });
    }
  }

  return layout(nodes, edges, roadmap.nodes);
}

function nodeHeight(spec: WorkSpec): number {
  // rough: title + ~ chars-per-line of description, big readable type
  const descLines = spec.description ? Math.ceil(spec.description.length / 34) : 0;
  return 56 + descLines * 26;
}

function layout(
  nodes: WorkFlowNode[],
  edges: Edge[],
  specs: WorkSpec[],
): { nodes: WorkFlowNode[]; edges: Edge[] } {
  const heights = new Map(specs.map((s) => [s.id, nodeHeight(s)]));
  const g = new Dagre.graphlib.Graph().setDefaultEdgeLabel(() => ({}));
  g.setGraph({ rankdir: "LR", nodesep: 48, ranksep: 130, marginx: 60, marginy: 60 });

  for (const n of nodes) g.setNode(n.id, { width: NODE_W, height: heights.get(n.id) ?? 80 });
  for (const e of edges) g.setEdge(e.source, e.target);

  Dagre.layout(g);

  const laidOut = nodes.map((n) => {
    const { x, y } = g.node(n.id);
    const h = heights.get(n.id) ?? 80;
    return {
      ...n,
      position: { x: x - NODE_W / 2, y: y - h / 2 },
      sourcePosition: Position.Right,
      targetPosition: Position.Left,
    };
  });

  return { nodes: laidOut, edges };
}
