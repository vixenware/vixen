// A lean CodeMirror 6 surface for vix source. (Highlighting via snark comes
// later; this playground is about the DAEMON, not the parser.)

import { useEffect, useRef } from "react";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";

export function Editor(props: { initial: string; onChange: (source: string) => void }) {
  const host = useRef<HTMLDivElement | null>(null);
  const view = useRef<EditorView | null>(null);
  // Keep the latest callback without rebuilding the editor.
  const onChange = useRef(props.onChange);
  onChange.current = props.onChange;

  useEffect(() => {
    if (!host.current) return;
    const state = EditorState.create({
      doc: props.initial,
      extensions: [
        lineNumbers(),
        history(),
        keymap.of([...defaultKeymap, ...historyKeymap, indentWithTab]),
        EditorView.updateListener.of((u) => {
          if (u.docChanged) onChange.current(u.state.doc.toString());
        }),
        EditorView.theme({}, { dark: true }),
      ],
    });
    view.current = new EditorView({ state, parent: host.current });
    return () => view.current?.destroy();
    // The editor owns the document after mount; `initial` is just the seed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return <div className="editor" ref={host} />;
}
