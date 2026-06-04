import yaml from "js-yaml";
import Dagre from "@dagrejs/dagre";
import { Position, type Edge, type Node } from "@xyflow/react";

// ---------------------------------------------------------------------------
// Data model — mirrors the YAML in `src/tree.yaml`.
// ---------------------------------------------------------------------------

export type Status = "done" | "in_progress" | "planned" | "idea";

export interface SkillSpec {
  id: string;
  title: string;
  status: Status;
  /** key into `categories` */
  category?: string;
  /** one-line teaser shown on the node */
  summary?: string;
  /** longer prose shown in the side panel */
  detail?: string;
  /** why this milestone helps convince funders */
  payoff?: string;
  /** ids of skills that must come first */
  requires?: string[];
}

export interface CategorySpec {
  label?: string;
  color: string;
}

export interface SkillTree {
  meta?: { title?: string; subtitle?: string };
  categories: Record<string, CategorySpec>;
  nodes: SkillSpec[];
}

export interface SkillNodeData extends Record<string, unknown> {
  spec: SkillSpec;
  category?: CategorySpec;
}

export type SkillFlowNode = Node<SkillNodeData, "skill">;

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

const DEFAULT_CATEGORY: CategorySpec = { label: "Uncategorized", color: "#7a8190" };

export function parseTree(source: string): SkillTree {
  const raw = yaml.load(source) as Partial<SkillTree> | undefined;
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
    for (const dep of n.requires ?? []) {
      if (!ids.has(dep)) {
        throw new Error(`node "${n.id}" requires unknown node "${dep}"`);
      }
    }
  }

  return {
    meta: raw.meta,
    categories: raw.categories ?? {},
    nodes: raw.nodes,
  };
}

// ---------------------------------------------------------------------------
// React Flow conversion + dagre layout
// ---------------------------------------------------------------------------

const NODE_W = 240;
const NODE_H = 108;

export function toFlow(tree: SkillTree): { nodes: SkillFlowNode[]; edges: Edge[] } {
  const nodes: SkillFlowNode[] = tree.nodes.map((spec) => ({
    id: spec.id,
    type: "skill",
    position: { x: 0, y: 0 },
    data: {
      spec,
      category: spec.category ? tree.categories[spec.category] ?? DEFAULT_CATEGORY : undefined,
    },
  }));

  const edges: Edge[] = [];
  for (const spec of tree.nodes) {
    for (const dep of spec.requires ?? []) {
      edges.push({
        id: `${dep}->${spec.id}`,
        source: dep,
        target: spec.id,
        animated: spec.status === "in_progress",
      });
    }
  }

  return layout(nodes, edges);
}

function layout(nodes: SkillFlowNode[], edges: Edge[]): { nodes: SkillFlowNode[]; edges: Edge[] } {
  const g = new Dagre.graphlib.Graph().setDefaultEdgeLabel(() => ({}));
  g.setGraph({ rankdir: "LR", nodesep: 36, ranksep: 96, marginx: 40, marginy: 40 });

  for (const n of nodes) g.setNode(n.id, { width: NODE_W, height: NODE_H });
  for (const e of edges) g.setEdge(e.source, e.target);

  Dagre.layout(g);

  const laidOut = nodes.map((n) => {
    const { x, y } = g.node(n.id);
    return {
      ...n,
      position: { x: x - NODE_W / 2, y: y - NODE_H / 2 },
      sourcePosition: Position.Right,
      targetPosition: Position.Left,
    };
  });

  return { nodes: laidOut, edges };
}
