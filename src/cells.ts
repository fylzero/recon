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

export function initialColumnWidth(column: ColumnMeta, charWidth: number) {
  const chars = Math.max(column.name.length + 2, isNumericColumn(column) ? 8 : 14);
  return Math.round(Math.min(Math.max(chars * charWidth + 24, 72), 320));
}
