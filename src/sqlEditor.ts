import { MySQL, PostgreSQL, SQLite, type SQLDialect } from "@codemirror/lang-sql";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { EditorView } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";
import type { Driver } from "./types";

export function sqlDialect(driver: Driver): SQLDialect {
  if (driver === "postgres") {
    return PostgreSQL;
  }
  return driver === "sqlite" ? SQLite : MySQL;
}

export const sqlEditorTheme = EditorView.theme(
  {
    "&": {
      height: "100%",
      color: "var(--text)",
      backgroundColor: "#0d1016",
      fontSize: "var(--editor-font-size)",
    },
    "&.cm-focused": { outline: "none" },
    ".cm-scroller": {
      fontFamily: "var(--editor-font-family)",
      lineHeight: "1.55",
    },
    ".cm-content": { caretColor: "var(--text)", padding: "0.45rem 0" },
    ".cm-gutters": {
      backgroundColor: "#0d1016",
      color: "var(--muted)",
      borderRight: "1px solid var(--border)",
    },
    ".cm-activeLine": { backgroundColor: "rgba(255, 255, 255, 0.03)" },
    ".cm-activeLineGutter": { backgroundColor: "transparent" },
    ".cm-selectionBackground, &.cm-focused .cm-selectionBackground": {
      backgroundColor: "#2a3344",
    },
    ".cm-cursor": { borderLeftColor: "var(--text)" },
    ".cm-tooltip": {
      backgroundColor: "var(--bg-raised)",
      border: "1px solid var(--border)",
      color: "var(--text)",
    },
    ".cm-tooltip-autocomplete > ul > li[aria-selected]": {
      backgroundColor: "var(--bg-active)",
      color: "var(--text)",
    },
    ".cm-placeholder": { color: "var(--muted)" },
  },
  { dark: true },
);

export const sqlHighlighting = syntaxHighlighting(
  HighlightStyle.define([
    { tag: t.keyword, color: "#c4a6ff" },
    { tag: [t.string, t.special(t.string)], color: "#3dd68c" },
    { tag: t.number, color: "#e6c07b" },
    { tag: t.bool, color: "#9ec1ff" },
    { tag: t.null, color: "#9ec1ff" },
    { tag: [t.lineComment, t.blockComment], color: "#6b7588", fontStyle: "italic" },
    { tag: t.typeName, color: "#7ee0c7" },
    { tag: [t.operator, t.punctuation], color: "#aab4c6" },
    { tag: t.special(t.name), color: "#f0a36b" },
    { tag: t.invalid, color: "var(--bad)" },
  ]),
);
