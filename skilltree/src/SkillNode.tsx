import { Handle, Position, type NodeProps } from "@xyflow/react";
import type { SkillFlowNode } from "./tree";

const STATUS_LABEL: Record<string, string> = {
  done: "Done",
  in_progress: "In progress",
  planned: "Planned",
  idea: "Idea",
};

export function SkillNode({ data, selected }: NodeProps<SkillFlowNode>) {
  const { spec, category } = data;
  const color = category?.color ?? "#7a8190";

  return (
    <div
      className={`skill-node status-${spec.status}`}
      data-selected={selected || undefined}
      style={{ "--cat": color } as React.CSSProperties}
    >
      <Handle type="target" position={Position.Left} />
      <header>
        <span className="dot" />
        <span className="title">{spec.title}</span>
      </header>
      {spec.summary && <p className="summary">{spec.summary}</p>}
      <footer>
        {spec.hands && <span className="hands" title="Good for extra hands (Hexcat)">✋ hands</span>}
        {category?.label && <span className="cat">{category.label}</span>}
        <span className="status">{STATUS_LABEL[spec.status] ?? spec.status}</span>
      </footer>
      <Handle type="source" position={Position.Right} />
    </div>
  );
}
