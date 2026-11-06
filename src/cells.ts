import type { Cell, ColumnMeta } from "./types";

const NUMERIC_TYPE = /INT|DECIMAL|NUMERIC|FLOAT|DOUBLE|REAL|SERIAL|MONEY|NUMBER/i;
const DISPLAY_LIMIT = 400;

export function isNumericColumn(column: ColumnMeta) {
  return NUMERIC_TYPE.test(column.typeName);
}

export function isBytes(value: Cell): value is { bytes: number; hex: string } {
  return typeof value === "object" && value !== null && "bytes" in value;
}

function bytesLabel(value: { bytes: number; hex: string }) {
  if (!value.bytes) {
    return "(empty)";
  }
  const more = value.hex.length / 2 < value.bytes ? "…" : "";
  return `0x${value.hex.toUpperCase()}${more}`;
}

export function cellText(value: Cell): string {
  if (value === null) {
    return "NULL";
  }
  if (isBytes(value)) {
    return bytesLabel(value);
  }
  return String(value);
}

export function cellDisplay(value: Cell): string {
  const text = cellText(value);
  const clipped = text.length > DISPLAY_LIMIT ? `${text.slice(0, DISPLAY_LIMIT)}…` : text;
  return clipped.replace(/\r?\n/g, " ↵ ").replace(/\t/g, " ");
}

export function cellTitle(value: Cell): string | undefined {
  if (value === null) {
    return undefined;
  }
  if (isBytes(value)) {
    return `${value.bytes.toLocaleString()} bytes`;
  }
  const text = String(value);
  return text.length > 40 || text.includes("\n") ? text.slice(0, 4000) : undefined;
}

export function cellCopyText(value: Cell): string {
  if (value === null) {
    return "NULL";
  }
  return cellText(value).replace(/\t/g, " ").replace(/\r?\n/g, " ");
}

export type CopyFormat = "tsv" | "csv" | "json";

const JSON_TYPE = /^JSONB?$/i;

/** Bytes as `\x` and hex, like exports write them. The grid only holds the start of long values, so those end in …. */
function bytesText(value: { bytes: number; hex: string }) {
  const more = value.hex.length / 2 < value.bytes ? "…" : "";
  return `\\x${value.hex.toLowerCase()}${more}`;
}

function csvField(value: Cell) {
  if (value === null) {
    return "";
  }
  const text = isBytes(value) ? bytesText(value) : String(value);
  return text === "" || /[",\r\n]|^\s|\s$/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

function jsonCell(value: Cell, column: ColumnMeta): unknown {
  if (isBytes(value)) {
    return bytesText(value);
  }
  if (typeof value === "string" && JSON_TYPE.test(column.typeName)) {
    try {
      return JSON.parse(value);
    } catch {
      return value;
    }
  }
  return value;
}

/** Repeated column names get `_2`, `_3`, and so on, so every JSON key is distinct. */
function uniqueNames(names: string[]) {
  const seen = new Set<string>();
  return names.map((name) => {
    let candidate = name;
    for (let next = 2; seen.has(candidate); next += 1) {
      candidate = `${name}_${next}`;
    }
    seen.add(candidate);
    return candidate;
  });
}

/** Rows as CSV with a header, or as a JSON array of objects, for the clipboard. */
export function copyText(format: "csv" | "json", columns: ColumnMeta[], rows: Cell[][]) {
  if (format === "csv") {
    const header = columns.map((column) => csvField(column.name)).join(",");
    return [header, ...rows.map((row) => row.map(csvField).join(","))].join("\n");
  }
  const keys = uniqueNames(columns.map((column) => column.name));
  const objects = rows.map((row) =>
    Object.fromEntries(keys.map((key, index) => [key, jsonCell(row[index] ?? null, columns[index])])),
  );
  return JSON.stringify(objects, null, 2);
}

export function initialColumnWidth(column: ColumnMeta, charWidth: number) {
  const chars = Math.max(column.name.length + 2, isNumericColumn(column) ? 8 : 14);
  return Math.round(Math.min(Math.max(chars * charWidth + 24, 72), 320));
}
