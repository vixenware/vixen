// A lean CodeMirror 6 surface for vix source, cross-linked to the demand
// graph: the graph selects source SPANS here, and cursor movement reports
// back so the graph can highlight the demand rows born from that code.
// (Snark-powered highlighting/intelligence comes next.)

import { useEffect, useRef } from "react";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";

export interface EditorApi {
  /// Select the half-open byte range and scroll it into view.
  selectSpan(start: number, end: number): void;
}

export function Editor(props: {
  initial: string;
  onChange: (source: string) => void;
  onCursor: (offset: number) => void;
  api: (api: EditorApi) => void;
}) {
  const host = useRef<HTMLDivElement | null>(null);
  // Keep the latest callbacks without rebuilding the editor.
  const onChange = useRef(props.onChange);
  onChange.current = props.onChange;
  const onCursor = useRef(props.onCursor);
  onCursor.current = props.onCursor;

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
          if (u.selectionSet) onCursor.current(u.state.selection.main.head);
        }),
        EditorView.theme({}, { dark: true }),
      ],
    });
    const v = new EditorView({ state, parent: host.current });
    props.api({
      selectSpan(start, end) {
        const len = v.state.doc.length;
        const anchor = Math.min(start, len);
        const head = Math.min(end, len);
        v.dispatch({
          selection: { anchor, head },
          effects: EditorView.scrollIntoView(anchor, { y: "center" }),
        });
        v.focus();
      },
    });
    return () => v.destroy();
    // The editor owns the document after mount; `initial` is just the seed.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return <div className="editor" ref={host} />;
}
