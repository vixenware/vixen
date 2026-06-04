import { Handle, Position, type NodeProps } from "@xyflow/react";
import type { WorkFlowNode } from "./tree";

export function SkillNode({ data }: NodeProps<WorkFlowNode>) {
  const { spec } = data;
  return (
    <div className="work-node">
      <Handle type="target" position={Position.Left} />
      <h2>{spec.title}</h2>
      {spec.description && <p>{spec.description}</p>}
      <Handle type="source" position={Position.Right} />
    </div>
  );
}
