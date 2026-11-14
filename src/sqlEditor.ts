import { MSSQL, MySQL, PostgreSQL, SQLite, type SQLDialect } from "@codemirror/lang-sql";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { EditorView } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";
import { editorChrome } from "./theme";
import type { Driver } from "./types";

export function sqlDialect(driver: Driver): SQLDialect {
  if (driver === "postgres") {
    return PostgreSQL;
  }
  if (driver === "sqlite") {
    return SQLite;
  }
  if (driver === "mssql") {
    return MSSQL;
  }
  return MySQL;
}

export const sqlEditorTheme = EditorView.theme(
  {
    "&": {
      height: "100%",
      color: "var(--text)",
      backgroundColor: "var(--bg-input)",
      fontSize: "var(--editor-font-size)",
    },
    ...editorChrome,
    "&.cm-focused": { outline: "none" },
    ".cm-scroller": {
      fontFamily: "var(--editor-font-family)",
      lineHeight: "1.55",
    },
    ".cm-content": { caretColor: "var(--text)", padding: "0.45rem 0" },
    ".cm-gutters": {
      backgroundColor: "var(--bg-input)",
      color: "var(--muted)",
      borderRight: "1px solid var(--border)",
    },
    ".cm-activeLine": { backgroundColor: "rgb(var(--tint) / 0.03)" },
    ".cm-activeLineGutter": { backgroundColor: "transparent" },
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
    { tag: t.keyword, color: "var(--syntax-keyword)" },
    { tag: [t.string, t.special(t.string)], color: "var(--syntax-string)" },
    { tag: t.number, color: "var(--syntax-number)" },
    { tag: t.bool, color: "var(--syntax-constant)" },
    { tag: t.null, color: "var(--syntax-constant)" },
    { tag: [t.lineComment, t.blockComment], color: "var(--syntax-comment)", fontStyle: "italic" },
    { tag: t.typeName, color: "var(--syntax-type)" },
    { tag: [t.operator, t.punctuation], color: "var(--syntax-operator)" },
    { tag: t.special(t.name), color: "var(--syntax-special)" },
    { tag: t.invalid, color: "var(--bad)" },
  ]),
);
