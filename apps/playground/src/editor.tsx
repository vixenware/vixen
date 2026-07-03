// The vix editor: CodeMirror 6 + snark-powered intelligence (syntax captures
// and symbol occurrences from vix's embedded grammar, via wasm) — cross-linked
// to the demand graph: the graph selects source SPANS here, cursor movement
// reports back so the graph can highlight the demand rows born from that code.

import { useEffect, useRef } from "react";
import { EditorState, RangeSetBuilder } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView, ViewPlugin, type ViewUpdate, keymap, lineNumbers } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { highlightsFor, occurrencesAt, whenReady } from "./intelligence";
import { byteToUtf16, utf16ToByte } from "./offsets";

export interface EditorApi {
  /// Select the half-open UTF-16 range and scroll it into view.
  selectSpan(start: number, end: number): void;
}

function capClass(capture: string): string {
  return `cap-${capture.replace(/\./g, "-")}`;
}

/// Syntax captures + occurrence highlights as one decoration set, recomputed
/// on doc/selection changes. Capture spans are BYTES; decorations are UTF-16.
const intelligence = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;

    constructor(view: EditorView) {
      this.decorations = this.compute(view.state);
    }

    update(u: ViewUpdate) {
      if (u.docChanged || u.selectionSet) {
        this.decorations = this.compute(u.state);
      }
    }

    compute(state: EditorState): DecorationSet {
      const source = state.doc.toString();
      const marks: Array<{ from: number; to: number; cls: string }> = [];
      for (const c of highlightsFor(source)) {
        marks.push({
          from: byteToUtf16(source, c.start),
          to: byteToUtf16(source, c.end),
          cls: capClass(c.capture),
        });
      }
      const cursorByte = utf16ToByte(source, state.selection.main.head);
      const occ = occurrencesAt(source, cursorByte);
      if (occ !== null) {
        for (const s of occ) {
          marks.push({
            from: byteToUtf16(source, s.start),
            to: byteToUtf16(source, s.end),
            cls: "occurrence",
          });
        }
      }
      // RangeSetBuilder wants sorted, and same-position marks compose.
      marks.sort((a, z) => a.from - z.from || a.to - z.to);
      const b = new RangeSetBuilder<Decoration>();
      for (const m of marks) {
        if (m.from < m.to) b.add(m.from, m.to, Decoration.mark({ class: m.cls }));
      }
      return b.finish();
    }
  },
  { decorations: (v) => v.decorations },
);

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
        intelligence,
        EditorView.updateListener.of((u) => {
          if (u.docChanged) onChange.current(u.state.doc.toString());
          if (u.selectionSet) onCursor.current(u.state.selection.main.head);
        }),
        EditorView.theme({}, { dark: true }),
      ],
    });
    const v = new EditorView({ state, parent: host.current });
    // Once the wasm intelligence loads, refresh decorations.
    void whenReady().then(() => {
      v.dispatch({ selection: v.state.selection });
    });
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
