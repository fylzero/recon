import type { Driver } from "./types";

export interface StatementRange {
  from: number;
  to: number;
  text: string;
}

function isIdentChar(char: string | undefined) {
  return char !== undefined && /[A-Za-z0-9_]/.test(char);
}

function skipQuoted(sql: string, start: number, quote: string, backslashEscapes: boolean) {
  let index = start + 1;
  while (index < sql.length) {
    const char = sql[index];
    if (backslashEscapes && char === "\\") {
      index += 2;
      continue;
    }
    if (char === quote) {
      if (sql[index + 1] === quote) {
        index += 2;
        continue;
      }
      return index + 1;
    }
    index += 1;
  }
  return sql.length;
}

function dollarTag(sql: string, start: number) {
  const match = /^\$([A-Za-z_][A-Za-z0-9_]*)?\$/.exec(sql.slice(start, start + 64));
  if (!match || isIdentChar(sql[start - 1])) {
    return null;
  }
  return match[0];
}

function pushRange(ranges: StatementRange[], sql: string, from: number, to: number) {
  const chunk = sql.slice(from, to);
  const leading = chunk.length - chunk.trimStart().length;
  const text = chunk.trim();
  if (text && !isOnlyComments(text)) {
    ranges.push({ from: from + leading, to: from + leading + text.length, text });
  }
}

function isOnlyComments(text: string) {
  return !stripComments(text).trim();
}

export function stripComments(sql: string) {
  return sql
    .replace(/\/\*[\s\S]*?\*\//g, " ")
    .replace(/--[^\n]*/g, " ")
    .replace(/^\s*#[^\n]*/gm, " ");
}

export function splitStatements(sql: string, driver: Driver): StatementRange[] {
  const ranges: StatementRange[] = [];
  const backslashEscapes = driver === "mysql";
  let start = 0;
  let index = 0;
  while (index < sql.length) {
    const char = sql[index];
    const next = sql[index + 1];
    if (char === "'" || char === '"' || char === "`") {
      index = skipQuoted(sql, index, char, backslashEscapes && char !== "`");
      continue;
    }
    if (char === "-" && next === "-") {
      const end = sql.indexOf("\n", index);
      index = end === -1 ? sql.length : end + 1;
      continue;
    }
    if (char === "#" && driver === "mysql") {
      const end = sql.indexOf("\n", index);
      index = end === -1 ? sql.length : end + 1;
      continue;
    }
    if (char === "/" && next === "*") {
      const end = sql.indexOf("*/", index + 2);
      index = end === -1 ? sql.length : end + 2;
      continue;
    }
    if (char === "$" && driver === "postgres") {
      const tag = dollarTag(sql, index);
      if (tag) {
        const end = sql.indexOf(tag, index + tag.length);
        index = end === -1 ? sql.length : end + tag.length;
        continue;
      }
    }
    if (char === ";") {
      pushRange(ranges, sql, start, index);
      start = index + 1;
    }
    index += 1;
  }
  pushRange(ranges, sql, start, sql.length);
  return ranges;
}

export function statementAt(ranges: StatementRange[], position: number) {
  if (!ranges.length) {
    return null;
  }
  const inside = ranges.find((range) => position >= range.from && position <= range.to);
  if (inside) {
    return inside;
  }
  let best: StatementRange | null = null;
  for (const range of ranges) {
    if (range.to <= position) {
      best = range;
    }
  }
  return best ?? ranges[0];
}
