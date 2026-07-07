import { Handle, Position, useStore } from "@xyflow/react";
import type { SkillNodeData } from "./types";

interface Props {
  data: SkillNodeData & { laneColor: string; unlocked: boolean };
}

const LOD_ZOOM = 0.55;

export default function SkillNode({ data }: Props) {
  const zoom = useStore((s) => s.transform[2]);
  const far = zoom < LOD_ZOOM;
  const { title, short, lane, status, laneColor, unlocked } = data;
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
      className={`skill-node ${status}${status === "goal" && unlocked ? " unlocked" : ""}${far ? " lod" : ""}`}
      style={{ ["--lane" as string]: laneColor }}
    >
      <Handle type="target" position={Position.Left} />
      <div className="skill-badge">{badge}</div>
      <div className="skill-body">
        {far ? (
          <div className="skill-short">{(short as string) ?? title}</div>
        ) : (
          <>
            <div className="skill-lane">{lane}</div>
            <div className="skill-title">{title}</div>
          </>
        )}
      </div>
      <Handle type="source" position={Position.Right} />
    </div>
  );
}
