/**
 * Markdown ソース編集（CodeMirror 6）。
 * MdLite (Ao-aoi/mdlite) の src/source.ts を、Scenote 向けに調整して移植したもの。
 * コードブロック内の言語別ハイライト（language-data）は軽量化のため含めていない。
 */
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { EditorSelection, EditorState, Prec } from "@codemirror/state";
import { drawSelection, EditorView, keymap, placeholder } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";
import { toggleMarker } from "./toggle";

export interface SourceOptions {
  parent: HTMLElement;
  text: string;
  placeholder?: string;
  ariaLabel?: string;
  onEdit: (text: string) => void;
  onBlur?: () => void;
}

/** 色は CSS 変数で指定し、ライト/ダークは CSS 側で切り替える。 */
const highlight = HighlightStyle.define([
  { tag: t.heading, color: "var(--syn-heading)", fontWeight: "700" },
  { tag: t.strong, fontWeight: "700" },
  { tag: t.emphasis, fontStyle: "italic" },
  { tag: t.strikethrough, textDecoration: "line-through" },
  { tag: [t.link, t.url], color: "var(--syn-link)" },
  { tag: [t.processingInstruction, t.meta, t.contentSeparator], color: "var(--syn-mark)" },
  { tag: t.monospace, color: "var(--syn-code)" },
  { tag: t.quote, color: "var(--syn-quote)" },
]);

const STRONG = ["**", "__"];
const EMPHASIS = ["*", "_"];

export class SourceEditor {
  readonly view: EditorView;

  constructor(opts: SourceOptions) {
    const toggle = (markers: string[]) => (view: EditorView) => {
      const sel = view.state.selection.main;
      const r = toggleMarker(view.state.doc.toString(), sel.from, sel.to, markers);
      view.dispatch({
        changes: r.changes,
        selection: EditorSelection.single(r.anchor, r.head),
        userEvent: "input.format",
        scrollIntoView: true,
      });
      return true;
    };

    const formatKeys = Prec.highest(
      keymap.of([
        { key: "Mod-b", run: toggle(STRONG) },
        { key: "Mod-i", run: toggle(EMPHASIS) },
        { key: "Mod-Shift-x", run: toggle(["~~"]) },
      ]),
    );

    this.view = new EditorView({
      parent: opts.parent,
      state: EditorState.create({
        doc: opts.text,
        extensions: [
          formatKeys,
          history(),
          drawSelection(),
          EditorView.lineWrapping,
          markdown({ base: markdownLanguage }),
          syntaxHighlighting(highlight),
          keymap.of([...defaultKeymap, ...historyKeymap, indentWithTab]),
          placeholder(opts.placeholder ?? ""),
          EditorView.contentAttributes.of({
            spellcheck: "false",
            "aria-label": opts.ariaLabel ?? "Markdown ソース",
          }),
          EditorView.updateListener.of((u) => {
            if (u.docChanged) opts.onEdit(u.state.doc.toString());
          }),
          EditorView.domEventHandlers({
            blur: () => {
              opts.onBlur?.();
            },
          }),
        ],
      }),
    });
  }

  get text(): string {
    return this.view.state.doc.toString();
  }

  setText(text: string): void {
    this.view.dispatch({ changes: { from: 0, to: this.view.state.doc.length, insert: text } });
  }

  focus(): void {
    this.view.focus();
  }

  destroy(): void {
    this.view.destroy();
  }
}
