import type { Cell } from "./types";

export interface PastedCells {
  /** Column names, when the cells came from a Recon grid. */
  columns: string[] | null;
  rows: Cell[][];
}

/**
 * The cells behind the grid's last copy. Copied text turns NULL into a word
 * and newlines and tabs into spaces, so a paste of that same text uses these.
 */
let lastCopy: (PastedCells & { text: string }) | null = null;

export function rememberCopy(text: string, columns: string[], rows: Cell[][]) {
  lastCopy = { text, columns, rows };
}

/**
 * The cells in clipboard text. Text with tabs splits into rows and columns,
 * like a spreadsheet copy. Other text is one value, so multi-line text and
 * JSON paste into a single cell. The word NULL is NULL, as the grid copies it.
 */
export function pastedCells(text: string): PastedCells | null {
  if (lastCopy && lastCopy.text === text) {
    return { columns: lastCopy.columns, rows: lastCopy.rows };
  }
  const trimmed = text.replace(/\r?\n$/, "");
  if (!trimmed) {
    return null;
  }
  const cell = (value: string): Cell => (value === "NULL" ? null : value);
  if (!trimmed.includes("\t")) {
    return { columns: null, rows: [[cell(trimmed)]] };
  }
  return { columns: null, rows: trimmed.split(/\r?\n/).map((line) => line.split("\t").map(cell)) };
}
