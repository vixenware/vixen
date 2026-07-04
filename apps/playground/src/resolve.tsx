import { useEffect, useMemo, useRef, useState } from "react";
import downgradeCascade from "./samples/rodin-downgrade-cascade.json";
import holeTrap from "./samples/rodin-hole-trap.json";

type RungVerdict =
  | "Selected"
  | "Unexplained"
  | { ExcludedByRequirement: { edges: string[] } }
  | { KilledByFact: { fact_id: string } };

interface RecordRung {
  version: string;
  verdict: RungVerdict;
}

interface RecordPackage {
  id: string;
  source: string;
  name: string;
  versions: string[];
}

interface RecordNode {
  pkg: string;
  version: string;
  features: string[];
  why: string[];
  ladder: RecordRung[];
}

interface RecordEdge {
  id: string;
  from: string;
  to: string;
  requirement: string;
}

interface RecordFact {
  id: string;
  kind: string;
  region: Array<{ pkg: string; versions: string }>;
  support_edges: string[];
  widened_from: number;
  widened_to: number;
  summary: string;
}

type SolveStep =
  | { Demand: { pkg: string; requirement: string } }
  | { Decree: { origin: "Root" | "User"; pkg: string; requirement: string } }
  | { Intersect: { pkg: string; before: string; applied: string; after: string } }
  | { ForcedPick: { pkg: string; version: string; reason: string } }
  | { Expand: { pkg: string; version: string; candidates_remaining: number; manifests_read: number } }
  | { Conflict: { empty_pkg: string; edges_involved: string[] } }
  | {
      Learn: {
        region: Array<{ pkg: string; versions: string }>;
        support_edges: string[];
        widened_from: number;
        widened_to: number;
      };
    }
  | { Solution: { packages: Array<{ pkg: string; version: string }> } };

interface SolveRecord {
  scenario: string;
  shape: string;
  packages: RecordPackage[];
  steps: SolveStep[];
  graph: { nodes: RecordNode[]; edges: RecordEdge[] };
  facts: RecordFact[];
}

const SAMPLES = [
  { label: "downgrade cascade", record: downgradeCascade as SolveRecord },
  { label: "hole trap", record: holeTrap as SolveRecord },
];

const NODE_W = 170;
const NODE_H = 58;
const LAYER_GAP = 250;
const ROW_GAP = 98;
const PAD = 38;

interface LayoutNode extends RecordNode {
  x: number;
  y: number;
}

function packageMap(record: SolveRecord): Map<string, RecordPackage> {
  return new Map(record.packages.map((pkg) => [pkg.id, pkg]));
}

function packageName(packages: Map<string, RecordPackage>, pkg: string): string {
  return packages.get(pkg)?.name ?? pkg;
}

function packageIdByName(packages: Map<string, RecordPackage>, name: string): string {
  return [...packages.values()].find((pkg) => pkg.name === name)?.id ?? name;
}

function edgeLabel(edge: RecordEdge, packages: Map<string, RecordPackage>): string {
  return `${packageName(packages, edge.from)} -> ${packageName(packages, edge.to)} ${edge.requirement}`;
}

function layout(record: SolveRecord): { nodes: LayoutNode[]; width: number; height: number } {
  const nodesById = new Map(record.graph.nodes.map((node) => [node.pkg, node]));
  const incoming = new Map(record.graph.nodes.map((node) => [node.pkg, 0]));
  const outgoing = new Map<string, string[]>();
  for (const edge of record.graph.edges) {
    incoming.set(edge.to, (incoming.get(edge.to) ?? 0) + 1);
    outgoing.set(edge.from, [...(outgoing.get(edge.from) ?? []), edge.to]);
  }

  const depth = new Map<string, number>();
  const queue = record.graph.nodes
    .filter((node) => (incoming.get(node.pkg) ?? 0) === 0)
    .map((node) => node.pkg);
  for (const pkg of queue) depth.set(pkg, 0);

  for (let i = 0; i < queue.length; i += 1) {
    const pkg = queue[i];
    const nextDepth = (depth.get(pkg) ?? 0) + 1;
    for (const child of outgoing.get(pkg) ?? []) {
      if ((depth.get(child) ?? -1) < nextDepth) {
        depth.set(child, nextDepth);
        queue.push(child);
      }
    }
  }

  for (const node of record.graph.nodes) {
    if (!depth.has(node.pkg)) depth.set(node.pkg, 0);
  }

  const layers = new Map<number, RecordNode[]>();
  for (const node of record.graph.nodes) {
    const d = depth.get(node.pkg) ?? 0;
    layers.set(d, [...(layers.get(d) ?? []), node]);
  }

  const laidOut: LayoutNode[] = [];
  for (const [d, layerNodes] of [...layers.entries()].sort(([a], [b]) => a - b)) {
    layerNodes.sort((left, right) => left.pkg.localeCompare(right.pkg));
    layerNodes.forEach((node, row) => {
      laidOut.push({
        ...node,
        x: PAD + d * LAYER_GAP,
        y: PAD + row * ROW_GAP,
      });
    });
  }

  const maxDepth = Math.max(0, ...[...depth.values()]);
  const maxRows = Math.max(1, ...[...layers.values()].map((layerNodes) => layerNodes.length));
  return {
    nodes: laidOut,
    width: PAD * 2 + NODE_W + maxDepth * LAYER_GAP,
    height: PAD * 2 + NODE_H + (maxRows - 1) * ROW_GAP,
  };
}

function stepSentence(step: SolveStep, packages: Map<string, RecordPackage>): string {
  if ("Demand" in step) {
    return `demanded ${step.Demand.pkg} ${step.Demand.requirement}`;
  }
  if ("Decree" in step) {
    const where = step.Decree.origin === "Root" ? "at the root manifest" : "by the user";
    return `decreed ${step.Decree.pkg} ${step.Decree.requirement} ${where}`;
  }
  if ("Intersect" in step) {
    return `${step.Intersect.pkg} narrowed from ${step.Intersect.before} by ${step.Intersect.applied} to ${step.Intersect.after}`;
  }
  if ("ForcedPick" in step) {
    return `picked ${step.ForcedPick.pkg} ${step.ForcedPick.version}: ${step.ForcedPick.reason}`;
  }
  if ("Expand" in step) {
    return `read ${step.Expand.pkg} ${step.Expand.version}; ${step.Expand.candidates_remaining} candidates remained`;
  }
  if ("Conflict" in step) {
    const edges = step.Conflict.edges_involved.map((edge) => edgeDisplay(edge, packages)).join(", ");
    return `${step.Conflict.empty_pkg} became empty${edges ? ` from ${edges}` : ""}`;
  }
  if ("Learn" in step) {
    const edges = step.Learn.support_edges.map((edge) => edgeDisplay(edge, packages)).join(", ");
    return `learned a dead region from ${edges || "pins only"}; widened ${step.Learn.widened_from} to ${step.Learn.widened_to}`;
  }
  return `solved ${step.Solution.packages.map((pkg) => `${pkg.pkg} ${pkg.version}`).join(", ")}`;
}

function stepPackages(step: SolveStep): string[] {
  if ("Demand" in step) return [step.Demand.pkg];
  if ("Decree" in step) return [step.Decree.pkg];
  if ("Intersect" in step) return [step.Intersect.pkg];
  if ("ForcedPick" in step) return [step.ForcedPick.pkg];
  if ("Expand" in step) return [step.Expand.pkg];
  if ("Conflict" in step) return [step.Conflict.empty_pkg];
  if ("Learn" in step) return step.Learn.region.map((term) => term.pkg);
  return step.Solution.packages.map((pkg) => pkg.pkg);
}

function edgeDisplay(edgeId: string, packages: Map<string, RecordPackage>): string {
  const body = edgeId.replace(/^edge:/, "");
  const [from, to] = body.split("->");
  return `${packageName(packages, from)} -> ${packageName(packages, to)}`;
}

function factForVerdict(verdict: RungVerdict): string | null {
  return typeof verdict === "object" && "KilledByFact" in verdict
    ? verdict.KilledByFact.fact_id
    : null;
}

function edgesForVerdict(verdict: RungVerdict): string[] {
  return typeof verdict === "object" && "ExcludedByRequirement" in verdict
    ? verdict.ExcludedByRequirement.edges
    : [];
}

function verdictText(verdict: RungVerdict, facts: Map<string, RecordFact>, packages: Map<string, RecordPackage>): string {
  if (verdict === "Selected") return "selected";
  if (verdict === "Unexplained") return "unexplained";
  if ("KilledByFact" in verdict) {
    const fact = facts.get(verdict.KilledByFact.fact_id);
    return fact ? `blocked by ${fact.id}: ${fact.summary}` : `blocked by ${verdict.KilledByFact.fact_id}`;
  }
  const edges = verdict.ExcludedByRequirement.edges.map((edge) => edgeDisplay(edge, packages)).join(", ");
  return `blocked by requirement${edges ? `: ${edges}` : ""}`;
}

export function ResolveView() {
  const [sample, setSample] = useState(0);
  const [selectedPkg, setSelectedPkg] = useState<string | null>(null);
  const [selectedFact, setSelectedFact] = useState<string | null>(null);
  const [stepPkgs, setStepPkgs] = useState<string[]>([]);
  const graphRef = useRef<HTMLDivElement>(null);

  const record = SAMPLES[sample].record;
  const packages = useMemo(() => packageMap(record), [record]);
  const facts = useMemo(() => new Map(record.facts.map((fact) => [fact.id, fact])), [record]);
  const graph = useMemo(() => layout(record), [record]);
  const rootNode =
    record.graph.nodes.find(
      (node) => !record.graph.edges.some((edge) => edge.to === node.pkg),
    ) ??
    record.graph.nodes[0] ??
    null;
  const selectedNode = record.graph.nodes.find((node) => node.pkg === selectedPkg) ?? rootNode;
  const activeFact = selectedFact ? facts.get(selectedFact) ?? null : null;
  const supportEdges = new Set(activeFact?.support_edges ?? []);
  const factRegion = new Set(activeFact?.region.map((term) => term.pkg) ?? []);
  const stepSet = new Set(stepPkgs.map((pkg) => packageIdByName(packages, pkg)));

  useEffect(() => {
    graphRef.current?.scrollTo({ left: 0, top: 0 });
  }, [record.scenario]);

  useEffect(() => {
    const resetScroll = () => graphRef.current?.scrollTo({ left: 0, top: 0 });
    window.addEventListener("resize", resetScroll);
    return () => window.removeEventListener("resize", resetScroll);
  }, []);

  function nodeClass(node: RecordNode): string {
    const inSupport = record.graph.edges.some(
      (edge) => supportEdges.has(edge.id) && (edge.from === node.pkg || edge.to === node.pkg),
    );
    const dimByFact = activeFact !== null && (!inSupport || (factRegion.has(packageName(packages, node.pkg)) && !inSupport));
    const dimByStep = stepSet.size > 0 && !stepSet.has(node.pkg);
    return [
      "resolve-node",
      node.pkg === selectedPkg ? "resolve-node-selected" : "",
      stepSet.has(node.pkg) ? "resolve-node-step" : "",
      dimByFact || dimByStep ? "resolve-dim" : "",
    ].join(" ");
  }

  return (
    <div className="resolve-view">
      <div className="resolve-head">
        <label>
          sample
          <select
            value={sample}
            onChange={(event) => {
              setSample(Number(event.target.value));
              setSelectedPkg(null);
              setSelectedFact(null);
              setStepPkgs([]);
            }}
          >
            {SAMPLES.map((entry, ix) => (
              <option key={entry.record.scenario} value={ix}>
                {entry.label}
              </option>
            ))}
          </select>
        </label>
        <span className="resolve-title">{record.scenario}</span>
      </div>

      <div className="resolve-main">
        <div className="resolve-graph-wrap" ref={graphRef}>
          <svg className="resolve-graph" width={graph.width} height={graph.height}>
            {record.graph.edges.map((edge) => {
              const from = graph.nodes.find((node) => node.pkg === edge.from);
              const to = graph.nodes.find((node) => node.pkg === edge.to);
              if (!from || !to) return null;
              const highlighted = supportEdges.has(edge.id);
              const dimmed = (activeFact !== null && !highlighted) || (stepSet.size > 0 && !stepSet.has(from.pkg) && !stepSet.has(to.pkg));
              const x1 = from.x + NODE_W;
              const y1 = from.y + NODE_H / 2;
              const x2 = to.x;
              const y2 = to.y + NODE_H / 2;
              const mid = (x1 + x2) / 2;
              return (
                <g key={edge.id} className={dimmed ? "resolve-edge resolve-dim" : "resolve-edge"}>
                  <path
                    className={highlighted ? "resolve-edge-line resolve-edge-hot" : "resolve-edge-line"}
                    d={`M ${x1} ${y1} C ${mid} ${y1}, ${mid} ${y2}, ${x2} ${y2}`}
                  />
                  <text className="resolve-edge-label" x={mid} y={(y1 + y2) / 2 - 5} textAnchor="middle">
                    {edge.requirement}
                  </text>
                  <title>{edgeLabel(edge, packages)}</title>
                </g>
              );
            })}
            {graph.nodes.map((node) => {
              const pkg = packages.get(node.pkg);
              return (
                <g
                  key={node.pkg}
                  className={nodeClass(node)}
                  onClick={() => setSelectedPkg(node.pkg)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter" || event.key === " ") setSelectedPkg(node.pkg);
                  }}
                  tabIndex={0}
                  role="button"
                >
                  <rect x={node.x} y={node.y} width={NODE_W} height={NODE_H} rx={6} />
                  <text x={node.x + 12} y={node.y + 24} className="resolve-node-label">
                    {pkg?.name ?? node.pkg}@{node.version}
                  </text>
                  <text x={node.x + 12} y={node.y + 43} className="resolve-node-sub">
                    {node.features.join(", ") || "no features"}
                  </text>
                  <title>{node.features.join(", ") || "no features"}</title>
                </g>
              );
            })}
          </svg>
        </div>

        <aside className="resolve-panel">
          {selectedNode && (
            <NodeLadder
              node={selectedNode}
              packages={packages}
              facts={facts}
              onFact={setSelectedFact}
              selectedFact={selectedFact}
            />
          )}
          <div className="resolve-facts">
            <h3>facts</h3>
            {record.facts.map((fact) => (
              <button
                key={fact.id}
                className={selectedFact === fact.id ? "resolve-fact resolve-fact-active" : "resolve-fact"}
                onClick={() => setSelectedFact(selectedFact === fact.id ? null : fact.id)}
              >
                <span>{fact.id}</span>
                <span>{fact.summary}</span>
              </button>
            ))}
          </div>
        </aside>
      </div>

      <details className="resolve-steps">
        <summary>steps</summary>
        <ol>
          {record.steps.map((step, ix) => (
            <li
              key={ix}
              onMouseEnter={() => setStepPkgs(stepPackages(step))}
              onMouseLeave={() => setStepPkgs([])}
              onClick={() => setStepPkgs(stepPackages(step))}
            >
              {stepSentence(step, packages)}
            </li>
          ))}
        </ol>
      </details>
    </div>
  );
}

function NodeLadder(props: {
  node: RecordNode;
  packages: Map<string, RecordPackage>;
  facts: Map<string, RecordFact>;
  onFact: (fact: string) => void;
  selectedFact: string | null;
}) {
  const pkg = props.packages.get(props.node.pkg);
  return (
    <div className="resolve-ladder">
      <h3>{pkg?.name ?? props.node.pkg}@{props.node.version}</h3>
      <div className="resolve-why">
        {props.node.why.map((why) => (
          <span key={why}>{why}</span>
        ))}
      </div>
      <div className="resolve-rungs">
        {props.node.ladder.map((rung) => {
          const fact = factForVerdict(rung.verdict);
          return (
            <button
              key={rung.version}
              className={rung.verdict === "Selected" ? "resolve-rung resolve-rung-selected" : "resolve-rung"}
              onClick={() => {
                if (fact) props.onFact(fact);
              }}
            >
              <span>{rung.version}</span>
              <span>{verdictText(rung.verdict, props.facts, props.packages)}</span>
              {edgesForVerdict(rung.verdict).length > 0 && (
                <span>{edgesForVerdict(rung.verdict).map((edge) => edgeDisplay(edge, props.packages)).join(", ")}</span>
              )}
              {fact && props.selectedFact === fact && <span>highlighting support</span>}
            </button>
          );
        })}
      </div>
    </div>
  );
}
