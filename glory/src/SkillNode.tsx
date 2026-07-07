import { Handle, Position, type NodeProps } from "@xyflow/react";
import type { SkillNodeData } from "./types";

interface Props {
  data: SkillNodeData & { laneColor: string; unlocked: boolean };
}

export default function SkillNode({ data }: Props & Partial<NodeProps>) {
  const { title, lane, status, laneColor, unlocked } = data;
  const badge =
    status === "done"
      ? "✦"
      : status === "active"
        ? "◉"
        : status === "parked"
          ? "⏸"
          : status === "goal"
            ? unlocked
              ? "⚡"
              : "🔒"
            : "·";
  return (
    <div
      className={`skill-node ${status}${status === "goal" && unlocked ? " unlocked" : ""}`}
      style={{ ["--lane" as string]: laneColor }}
    >
      <Handle type="target" position={Position.Left} />
      <div className="skill-badge">{badge}</div>
      <div className="skill-body">
        <div className="skill-lane">{lane}</div>
        <div className="skill-title">{title}</div>
      </div>
      <Handle type="source" position={Position.Right} />
    </div>
  );
}
