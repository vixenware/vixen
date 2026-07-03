// The inspector: click a demand row, see everything the events know about it
// — the command grammar's description levels, the resolved argv, fn args, and
// the OUTPUT TREE of a flushed run (artifacts, path by path). For a Spawn, the
// paired Exec (same run id) contributes outputs; for an Exec, the paired
// Spawn contributes describe/argv.

import type { DemandEvent } from "./generated/daemon";

function pair(events: DemandEvent[], run: bigint, tag: "Created" | "Scheduled" | "Finished"): DemandEvent | null {
  for (const e of events) {
    if (e.tag === tag && e.run === run) return e;
  }
  return null;
}

export function Inspector(props: {
  event: DemandEvent;
  events: DemandEvent[];
  onClose: () => void;
}) {
  const { event: e, events, onClose } = props;

  let title = "";
  let describe: string[] = [];
  let argv: string[] | null = null;
  let args: [string, string][] = [];
  let outputs: [string, string][] = [];

  switch (e.tag) {
    case "Miss":
    case "Hit": {
      title = `fn ${e.func} — ${e.tag === "Miss" ? "evaluated" : "memo hit"}`;
      args = e.args;
      break;
    }
    case "Created":
    case "Scheduled":
    case "Finished": {
      const created = e.tag === "Created" ? e : pair(events, e.run, "Created");
      const finished = e.tag === "Finished" ? e : pair(events, e.run, "Finished");
      const scheduled = e.tag === "Scheduled" ? e : pair(events, e.run, "Scheduled");
      const state =
        finished !== null ? "finished" : scheduled !== null ? "scheduled" : "created";
      title = `run ${e.run} — ${e.command}, ${state}`;
      if (created?.tag === "Created") {
        describe = created.describe;
        argv = created.argv;
      }
      if (finished?.tag === "Finished") outputs = finished.outputs;
      break;
    }
    case "Observation":
      title = e.key;
      break;
    case "Done":
      title = "done";
      break;
    case "Failed":
      title = "failed";
      break;
  }

  return (
    <div className="inspector">
      <div className="inspector-head">
        <span className="inspector-title">{title}</span>
        <button className="inspector-close" onClick={onClose}>
          ×
        </button>
      </div>
      {describe.length > 0 && (
        <ul className="inspector-describe">
          {describe.slice(0, -1).map((d, i) => (
            <li key={i} className={i === 0 ? "d0" : "d1"}>
              {d}
            </li>
          ))}
        </ul>
      )}
      {args.length > 0 && (
        <table className="inspector-args">
          <tbody>
            {args.map(([n, v]) => (
              <tr key={n}>
                <td className="arg-name">{n}</td>
                <td className="arg-value">{v}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {argv !== null && <pre className="inspector-argv">{argv.join(" ")}</pre>}
      {outputs.length > 0 && (
        <div className="inspector-outputs">
          <div className="outputs-head">outputs</div>
          {outputs.map(([path, hash]) => (
            <div key={path} className="output-row">
              <span className="output-path">{path}</span>
              <span className="output-hash">{hash}</span>
            </div>
          ))}
        </div>
      )}
      {(e.tag === "Created" || e.tag === "Scheduled") && outputs.length === 0 && (
        <p className="inspector-note">
          {pair(events, e.run, "Scheduled") === null
            ? "Thunk only — never demanded. You did not pay for this run."
            : "Scheduled by a path projection; the full tree was never needed, so no completion is logged. Projection doesn't force."}
        </p>
      )}
    </div>
  );
}
