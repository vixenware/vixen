export type Status = "done" | "active" | "queued" | "parked" | "goal";

export interface SkillNodeData {
  id: string;
  title: string;
  lane: string;
  status: Status;
  desc: string;
  evidence?: string[];
  deps?: string[];
  [key: string]: unknown;
}

export interface Progress {
  updated: string;
  meta: {
    title: string;
    subtitle: string;
    trunk: string;
  };
  lanes: Record<string, string>; // lane id -> accent color
  nodes: SkillNodeData[];
}
