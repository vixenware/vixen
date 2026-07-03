import { useRef, useState } from "react";
import { Editor, type EditorApi } from "./editor";
import { connect, startEval, type EvalSession } from "./daemon";
import type { DaemonClient, DemandEvent } from "./generated/daemon";
import { DemandGraph } from "./graph";
import { Inspector } from "./inspector";
import { byteToUtf16, utf16ToByte } from "./offsets";
import luaSample from "./samples/lua.vix?raw";

type Status = "idle" | "connecting" | "running" | "stepping" | "done" | "failed";

const DEFAULT_URL = "ws://127.0.0.1:4177";

export function App() {
  const source = useRef(luaSample);
  const client = useRef<DaemonClient | null>(null);
  const session = useRef<EvalSession | null>(null);
  const editor = useRef<EditorApi | null>(null);
  // The exact source the daemon evaluated — spans index into THIS, not into
  // whatever the editor holds now.
  const evalSource = useRef(luaSample);

  const [url, setUrl] = useState(DEFAULT_URL);
  const [entry, setEntry] = useState("lua");
  const [stepMode, setStepMode] = useState(false);
  const [status, setStatus] = useState<Status>("idle");
  const [events, setEvents] = useState<DemandEvent[]>([]);
  const [outcome, setOutcome] = useState<string | null>(null);
  // The cursor position in BYTE space (converted from CodeMirror's UTF-16),
  // comparable against event spans directly.
  const [cursor, setCursor] = useState<number | null>(null);
  const [inspected, setInspected] = useState<DemandEvent | null>(null);
  const [showAll, setShowAll] = useState(false);

  const busy = status === "connecting" || status === "running" || status === "stepping";

  async function demand() {
    setEvents([]);
    setOutcome(null);
    setInspected(null);
    setStatus("connecting");
    try {
      if (!client.current) {
        client.current = await connect(url);
      }
      evalSource.current = source.current;
      const s = startEval(client.current, {
        source: evalSource.current,
        entry,
        mode: { tag: stepMode ? "Step" : "Run" },
      });
      session.current = s;
      setStatus(stepMode ? "stepping" : "running");

      // Once Done/Failed arrived, the request scope is ending; the runtime
      // resets still-open channels (spec: rpc.request.scope.terminal) and the
      // event iterator may throw. After the terminal event that's just
      // shutdown, not an error.
      let terminal = false;
      try {
        for await (const e of s.events) {
          setEvents((prev) => [...prev, e]);
          if (e.tag === "Done") {
            terminal = true;
            setOutcome(e.result);
            setStatus("done");
          } else if (e.tag === "Failed") {
            terminal = true;
            setOutcome(e.error);
            setStatus("failed");
          }
        }
      } catch (err) {
        if (!terminal) throw err;
      }
      s.control.close();
      try {
        await s.finished;
      } catch (err) {
        if (!terminal) throw err;
      }
    } catch (err) {
      // A dead daemon or dropped socket: surface it, forget the client.
      client.current = null;
      setOutcome(String(err));
      setStatus("failed");
    } finally {
      session.current = null;
    }
  }

  function step() {
    void session.current?.control.send({ tag: "Step" });
  }

  function resume() {
    void session.current?.control.send({ tag: "Resume" });
    setStatus("running");
  }

  return (
    <div className="shell">
      <header>
        <span className="brand">vixen</span>
        <span className="brand-sub">demand, observed</span>
        <div className="conn">
          <input
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            spellCheck={false}
            title="vixd websocket address"
          />
        </div>
      </header>
      <main>
        <section className="pane editor-pane">
          <Editor
            initial={luaSample}
            onChange={(s) => (source.current = s)}
            onCursor={(utf16) => setCursor(utf16ToByte(evalSource.current, utf16))}
            api={(a) => (editor.current = a)}
          />
        </section>
        <section className="pane demand-pane">
          <div className="toolbar">
            <label className="entry">
              entry
              <input value={entry} onChange={(e) => setEntry(e.target.value)} spellCheck={false} />
            </label>
            <label className="steptoggle">
              <input
                type="checkbox"
                checked={showAll}
                onChange={(e) => setShowAll(e.target.checked)}
              />
              all
            </label>
            <label className="steptoggle">
              <input
                type="checkbox"
                checked={stepMode}
                onChange={(e) => setStepMode(e.target.checked)}
                disabled={busy}
              />
              step
            </label>
            <button className="primary" onClick={() => void demand()} disabled={busy}>
              demand
            </button>
            {status === "stepping" && (
              <>
                <button onClick={step}>step</button>
                <button onClick={resume}>resume</button>
              </>
            )}
            <span className={`status status-${status}`}>{status}</span>
          </div>
          <div className="graph-scroll">
            {events.length === 0 ? (
              <p className="hint">
                Run <code>vixd</code> locally, then demand an entry point. Every memo miss/hit,
                spawn, exec serving class and observation streams here as it happens — in step
                mode, one demand event per step.
              </p>
            ) : (
              <DemandGraph
                events={events}
                settled={status === "done" || status === "failed"}
                showAll={showAll}
                cursor={cursor}
                onSelect={(e) => {
                  setInspected(e);
                  const span =
                    e.tag === "Done" || e.tag === "Failed" || e.tag === "Observation"
                      ? null
                      : e.span;
                  if (span) {
                    editor.current?.selectSpan(
                      byteToUtf16(evalSource.current, span.start),
                      byteToUtf16(evalSource.current, span.end),
                    );
                  }
                }}
              />
            )}
          </div>
          {inspected !== null && (
            <Inspector event={inspected} events={events} onClose={() => setInspected(null)} />
          )}
          {outcome !== null && (
            <div className={`outcome outcome-${status}`}>
              <pre>{outcome}</pre>
            </div>
          )}
        </section>
      </main>
    </div>
  );
}
