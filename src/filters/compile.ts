import {
  MAX_LIST_VALUES,
  type DatePeriod,
  type DateUnit,
  type FilterCondition,
  type FilterGroup,
  type FilterKind,
  type FilterNode,
  type FilterScalar,
  type MatchMode,
} from "./model";
import { editorFor, isAllowed, operatorLabel, operatorShort, PERIOD_LABELS, UNIT_LABELS } from "./operators";

export interface FilterColumn {
  name: string;
  kind: FilterKind;
  nullable: boolean;
  enumValues: string[];
  indexed: boolean;
  dataType: string;
}

export type WireOp =
  | "eq" | "neq" | "gt" | "gte" | "lt" | "lte" | "between" | "notBetween" | "in" | "notIn"
  | "contains" | "notContains" | "startsWith" | "endsWith" | "isNull" | "isNotNull" | "isTrue" | "isFalse"
  | "isEmpty" | "isNotEmpty";

export type WireValue = string | boolean | null;

export type WireNode =
  | { kind: "group"; match: MatchMode; children: WireNode[] }
  | { kind: "condition"; id: string; column: string; op: WireOp; values: WireValue[] };

export interface CompiledFilter {
  /** What the backend receives; null when nothing applies. */
  wire: WireNode | null;
  /** Changes only when the query would change. */
  key: string;
  issues: Map<string, string>;
  /** Conditions still being filled in, which are left out without an error. */
  drafts: Set<string>;
  appliedCount: number;
  summary: string;
  /** The applied conditions laid out like the filter panel, for showing them outside it. */
  preview: FilterPreviewGroup | null;
  /** Conditions exist but the table's columns haven't loaded yet. */
  pending: boolean;
}

export interface FilterPreviewCondition {
  kind: "condition";
  id: string;
  column: string;
  columnKind: FilterKind;
  /** As worded in the operator menu, such as `> greater than`. */
  operator: string;
  /** As worded inside a sentence, such as `>`. */
  shortOperator: string;
  /** Empty for operators that take no value, such as “is NULL”. */
  value: string;
}

export interface FilterPreviewGroup {
  kind: "group";
  id: string;
  match: MatchMode;
  children: FilterPreviewNode[];
}

export type FilterPreviewNode = FilterPreviewCondition | FilterPreviewGroup;

const NUMBER_RE = /^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/;
const DATE_RE = /^(\d{4})-(\d{2})-(\d{2})$/;
const DATETIME_RE = /^(\d{4}-\d{2}-\d{2})[ T](\d{2}):(\d{2})(?::(\d{2})(\.\d{1,6})?)?$/;
const TIME_RE = /^(\d{2}):(\d{2})(?::(\d{2})(\.\d{1,6})?)?$/;
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const MAX_VALUE_LENGTH = 10_000;
const SUMMARY_LIST_ITEMS = 4;

class Draft {}

class Issue {
  constructor(readonly message: string) {}
}

const DRAFT = new Draft();

function pad(value: number) {
  return String(value).padStart(2, "0");
}

export function formatDate(date: Date) {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

function formatStamp(date: Date) {
  return `${formatDate(date)} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

export function isValidDate(text: string) {
  const match = DATE_RE.exec(text);
  if (!match) {
    return false;
  }
  const [year, month, day] = [Number(match[1]), Number(match[2]), Number(match[3])];
  const date = new Date(year, month - 1, day);
  return date.getFullYear() === year && date.getMonth() === month - 1 && date.getDate() === day;
}

function localDate(text: string) {
  const [year, month, day] = text.split("-").map(Number);
  return new Date(year, month - 1, day);
}

function addDays(text: string, days: number) {
  const date = localDate(text);
  date.setDate(date.getDate() + days);
  return formatDate(date);
}

function anchorDate(anchor: "today" | "yesterday" | "tomorrow", now: Date) {
  const today = formatDate(now);
  return anchor === "today" ? today : addDays(today, anchor === "yesterday" ? -1 : 1);
}

interface DatePoint {
  date: string;
  /** `HH:MM:SS`, when a time was given. */
  time?: string;
  /** Seconds were typed, so the value names one instant rather than a whole minute. */
  exact: boolean;
}

function scalarText(value: FilterScalar): string {
  if (typeof value === "string") {
    return value;
  }
  return value.kind === "absolute" ? value.value : "";
}

/** `YYYY-MM-DD`, or a typed `M/D/YYYY` that the date picker may store. */
function parseDateText(text: string): string | null {
  if (isValidDate(text)) {
    return text;
  }
  const us = /^(\d{1,2})\/(\d{1,2})\/(\d{4})$/.exec(text);
  if (!us) {
    return null;
  }
  const iso = `${us[3]}-${pad(Number(us[1]))}-${pad(Number(us[2]))}`;
  return isValidDate(iso) ? iso : null;
}

function datePoint(value: FilterScalar, kind: FilterKind, now: Date): DatePoint {
  if (typeof value !== "string" && value.kind === "relative") {
    return { date: anchorDate(value.anchor, now), exact: false };
  }
  const text = scalarText(value).trim();
  if (!text) {
    throw DRAFT;
  }
  const date = parseDateText(text);
  if (date) {
    return { date, exact: false };
  }
  const match = kind === "datetime" ? DATETIME_RE.exec(text) : null;
  if (match && isValidDate(match[1]) && Number(match[2]) < 24 && Number(match[3]) < 60 && Number(match[4] ?? 0) < 60) {
    const seconds = match[4] ?? "00";
    return { date: match[1], time: `${match[2]}:${match[3]}:${seconds}${match[5] ?? ""}`, exact: match[4] !== undefined };
  }
  throw new Issue(kind === "datetime" ? "Enter a date, or a date and time." : "Enter a date like 2026-09-27.");
}

function stampOf(point: DatePoint) {
  return `${point.date} ${point.time ?? "00:00:00"}`;
}

/** The first instant after the point: the next minute for a typed time, the next day for a date. */
function endOf(point: DatePoint) {
  if (!point.time) {
    return `${addDays(point.date, 1)} 00:00:00`;
  }
  const [hours, minutes] = point.time.split(":").map(Number);
  const date = localDate(point.date);
  date.setHours(hours, minutes + 1, 0, 0);
  return formatStamp(date);
}

function lastNRange(amount: number, unit: DateUnit, now: Date): [string, string] {
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const start = new Date(today);
  if (unit === "day") {
    start.setDate(start.getDate() - (amount - 1));
  } else if (unit === "week") {
    start.setDate(start.getDate() - (amount * 7 - 1));
  } else if (unit === "month") {
    start.setMonth(start.getMonth() - amount);
    start.setDate(start.getDate() + 1);
  } else {
    start.setFullYear(start.getFullYear() - amount);
    start.setDate(start.getDate() + 1);
  }
  return [formatDate(start), formatDate(today)];
}

/** Weeks start on Monday. */
export function periodRange(period: DatePeriod, now: Date): [string, string] {
  const year = now.getFullYear();
  const month = now.getMonth();
  switch (period) {
    case "thisWeek":
    case "lastWeek": {
      const monday = new Date(year, month, now.getDate() - ((now.getDay() + 6) % 7));
      if (period === "lastWeek") {
        monday.setDate(monday.getDate() - 7);
      }
      const sunday = new Date(monday);
      sunday.setDate(sunday.getDate() + 6);
      return [formatDate(monday), formatDate(sunday)];
    }
    case "thisMonth":
      return [formatDate(new Date(year, month, 1)), formatDate(new Date(year, month + 1, 0))];
    case "lastMonth":
      return [formatDate(new Date(year, month - 1, 1)), formatDate(new Date(year, month, 0))];
    case "thisYear":
      return [`${year}-01-01`, `${year}-12-31`];
    case "lastYear":
      return [`${year - 1}-01-01`, `${year - 1}-12-31`];
  }
}

function parseScalar(kind: FilterKind, value: FilterScalar): string {
  const raw = scalarText(value);
  const exactText = kind === "text" || kind === "enum" || kind === "other" || kind === "json";
  const text = exactText ? raw : raw.trim();
  // Spaces alone are a real text value; only a field left completely empty is unfinished.
  if (!text) {
    throw DRAFT;
  }
  if (text.length > MAX_VALUE_LENGTH) {
    throw new Issue(`Values can be at most ${MAX_VALUE_LENGTH.toLocaleString()} characters.`);
  }
  switch (kind) {
    case "number":
      if (!NUMBER_RE.test(text)) {
        throw new Issue(`“${text}” is not a number.`);
      }
      return text;
    case "time": {
      const match = TIME_RE.exec(text);
      if (!match || Number(match[1]) > 23 || Number(match[2]) > 59 || Number(match[3] ?? 0) > 59) {
        throw new Issue("Enter a time like 14:30.");
      }
      return text;
    }
    case "uuid":
      if (!UUID_RE.test(text)) {
        throw new Issue("Enter a full UUID, like 0f8fad5b-d9cb-469f-a165-70867728950e.");
      }
      return text;
    default:
      return text;
  }
}

function condition(id: string, column: string, op: WireOp, values: WireValue[]): WireNode {
  return { kind: "condition", id, column, op, values };
}

function range(id: string, column: string, from: string, to: string, inclusiveEnd = false): WireNode {
  return {
    kind: "group",
    match: "all",
    children: [condition(id, column, "gte", [from]), condition(id, column, inclusiveEnd ? "lte" : "lt", [to])],
  };
}

function dateWire(node: FilterCondition, kind: FilterKind, now: Date): WireNode {
  const { id, column, operator, value } = node;
  const dateOnly = kind === "date";
  if (value.type === "lastN" || value.type === "period") {
    if (value.type === "lastN" && (!Number.isInteger(value.amount) || value.amount < 1 || value.amount > 10_000)) {
      throw new Issue("Enter a whole number of at least 1.");
    }
    const [start, end] = value.type === "lastN" ? lastNRange(value.amount, value.unit, now) : periodRange(value.period, now);
    return dateOnly
      ? condition(id, column, "between", [start, end])
      : range(id, column, `${start} 00:00:00`, `${addDays(end, 1)} 00:00:00`);
  }
  if (value.type === "range") {
    const from = datePoint(value.from, kind, now);
    const to = datePoint(value.to, kind, now);
    if (stampOf(from) > stampOf(to)) {
      throw new Issue("The first date is after the second, so no rows can match.");
    }
    if (dateOnly) {
      return condition(id, column, "between", [from.date, to.date]);
    }
    return to.exact ? range(id, column, stampOf(from), stampOf(to), true) : range(id, column, stampOf(from), endOf(to));
  }
  if (value.type !== "single") {
    throw DRAFT;
  }
  const point = datePoint(value.value, kind, now);
  if (dateOnly) {
    return condition(id, column, operator as WireOp, [point.date]);
  }
  const start = stampOf(point);
  switch (operator) {
    case "eq":
      return point.exact ? condition(id, column, "eq", [start]) : range(id, column, start, endOf(point));
    case "lt":
      return condition(id, column, "lt", [start]);
    case "gt":
      return point.exact ? condition(id, column, "gt", [start]) : condition(id, column, "gte", [endOf(point)]);
    case "lte":
      return point.exact ? condition(id, column, "lte", [start]) : condition(id, column, "lt", [endOf(point)]);
    default:
      return condition(id, column, "gte", [start]);
  }
}

function conditionWire(node: FilterCondition, column: FilterColumn, now: Date): WireNode {
  const { id, operator, value } = node;
  const kind = column.kind;
  const editor = editorFor(operator);
  if (editor === "none") {
    return condition(id, node.column, operator as WireOp, []);
  }
  if (kind === "date" || kind === "datetime") {
    return dateWire(node, kind, now);
  }
  if (editor === "single" && value.type === "single") {
    return condition(id, node.column, operator as WireOp, [parseScalar(kind, value.value)]);
  }
  if (editor === "range" && value.type === "range") {
    const from = parseScalar(kind, value.from);
    const to = parseScalar(kind, value.to);
    if (kind === "number" && Number(from) > Number(to)) {
      throw new Issue("The first number is larger than the second, so no rows can match.");
    }
    return condition(id, node.column, operator as WireOp, [from, to]);
  }
  if (editor === "list" && value.type === "list") {
    const values = value.values.filter((item) => item.trim()).map((item) => parseScalar(kind, item));
    if (!values.length) {
      throw DRAFT;
    }
    if (values.length > MAX_LIST_VALUES) {
      throw new Issue(`Use at most ${MAX_LIST_VALUES.toLocaleString()} values.`);
    }
    return condition(id, node.column, operator as WireOp, [...new Set(values)]);
  }
  throw DRAFT;
}

/** Quotes text whose spaces would otherwise be invisible, such as `" "` or `"a "`. */
function textSummary(text: string) {
  return /^\s|\s$/.test(text) ? `"${text}"` : text;
}

function scalarSummary(value: FilterScalar, kind: FilterKind) {
  if (typeof value !== "string" && value.kind === "relative") {
    return value.anchor;
  }
  const text = scalarText(value);
  return kind === "date" || kind === "datetime" ? text.replace("T", " ") : textSummary(text);
}

/** The value as a reader sees it, or an empty string for operators without one. */
function valueSummary(node: FilterCondition, kind: FilterKind) {
  const value = node.value;
  switch (value.type) {
    case "none":
      return "";
    case "single":
      return scalarSummary(value.value, kind);
    case "range":
      return `${scalarSummary(value.from, kind)} and ${scalarSummary(value.to, kind)}`;
    case "list": {
      const shown = value.values.slice(0, SUMMARY_LIST_ITEMS).map(textSummary).join(", ");
      const more = value.values.length - SUMMARY_LIST_ITEMS;
      return `${shown}${more > 0 ? ` +${more} more` : ""}`;
    }
    case "lastN": {
      const [one, many] = UNIT_LABELS[value.unit];
      return `${value.amount} ${value.amount === 1 ? one : many}`;
    }
    case "period":
      return PERIOD_LABELS[value.period];
  }
}

export function conditionSummary(node: FilterCondition, kind: FilterKind) {
  const value = valueSummary(node, kind);
  return `${node.column} ${operatorShort(kind, node.operator)}${value ? ` ${value}` : ""}`;
}

interface Part {
  wire: WireNode;
  summary: string;
  preview: FilterPreviewNode;
}

export function compileFilter(
  root: FilterGroup,
  columns: Map<string, FilterColumn> | null,
  options: { now?: Date; columnsError?: string } = {},
): CompiledFilter {
  const now = options.now ?? new Date();
  const issues = new Map<string, string>();
  const drafts = new Set<string>();
  let appliedCount = 0;
  let pending = false;

  const visitCondition = (node: FilterCondition): Part | null => {
    if (!node.enabled) {
      return null;
    }
    if (!node.column) {
      drafts.add(node.id);
      return null;
    }
    if (!columns) {
      pending = true;
      return null;
    }
    const column = columns.get(node.column);
    if (!column) {
      issues.set(
        node.id,
        options.columnsError
          ? `The table's columns couldn't be loaded: ${options.columnsError}`
          : `This table has no column named “${node.column}”.`,
      );
      return null;
    }
    if (!isAllowed(column.kind, node.operator)) {
      issues.set(node.id, `“${operatorShort(column.kind, node.operator)}” can't be used with ${node.column}.`);
      return null;
    }
    try {
      const wire = conditionWire(node, column, now);
      appliedCount += 1;
      const preview: FilterPreviewCondition = {
        kind: "condition",
        id: node.id,
        column: node.column,
        columnKind: column.kind,
        operator: operatorLabel(column.kind, node.operator),
        shortOperator: operatorShort(column.kind, node.operator),
        value: valueSummary(node, column.kind),
      };
      return { wire, summary: conditionSummary(node, column.kind), preview };
    } catch (err) {
      if (err instanceof Issue) {
        issues.set(node.id, err.message);
      } else if (err === DRAFT) {
        drafts.add(node.id);
      } else {
        throw err;
      }
      return null;
    }
  };

  const visitGroup = (group: FilterGroup, top: boolean): Part | null => {
    if (!group.enabled) {
      return null;
    }
    const parts = group.children
      .map((child: FilterNode) => (child.kind === "group" ? visitGroup(child, false) : visitCondition(child)))
      .filter((part): part is Part => part !== null);
    if (parts.length <= 1) {
      return parts[0] ?? null;
    }
    const joined = parts.map((part) => part.summary).join(group.match === "all" ? " and " : " or ");
    return {
      wire: { kind: "group", match: group.match, children: parts.map((part) => part.wire) },
      summary: top ? joined : `(${joined})`,
      preview: { kind: "group", id: group.id, match: group.match, children: parts.map((part) => part.preview) },
    };
  };

  const result = visitGroup(root, true);
  return {
    wire: result?.wire ?? null,
    key: result ? JSON.stringify(result.wire) : "",
    issues,
    drafts,
    appliedCount,
    summary: result?.summary ?? "",
    preview: previewRoot(root, result?.preview ?? null),
    pending,
  };
}

/** The preview always starts from a group, even when only one condition applies. */
function previewRoot(root: FilterGroup, preview: FilterPreviewNode | null): FilterPreviewGroup | null {
  if (!preview) {
    return null;
  }
  return preview.kind === "group" ? preview : { kind: "group", id: root.id, match: root.match, children: [preview] };
}

function previewCondition(node: FilterCondition, columns: Map<string, FilterColumn> | null): FilterPreviewCondition | null {
  if (!node.enabled || !node.column) {
    return null;
  }
  const kind = columns?.get(node.column)?.kind ?? "other";
  return {
    kind: "condition",
    id: node.id,
    column: node.column,
    columnKind: kind,
    operator: operatorLabel(kind, node.operator),
    shortOperator: operatorShort(kind, node.operator),
    value: valueSummary(node, kind),
  };
}

function previewGroup(group: FilterGroup, columns: Map<string, FilterColumn> | null): FilterPreviewGroup | null {
  if (!group.enabled) {
    return null;
  }
  const children = group.children
    .map((child) => (child.kind === "group" ? previewGroup(child, columns) : previewCondition(child, columns)))
    .filter((child): child is FilterPreviewNode => child !== null);
  if (!children.length) {
    return null;
  }
  return { kind: "group", id: group.id, match: group.match, children };
}

/**
 * Boxed filter chip for the toolbar and tab icon, including rows that are still
 * drafts so a minimized panel still shows what the tab is filtering on.
 */
export function previewFilter(root: FilterGroup, columns: Map<string, FilterColumn> | null): FilterPreviewGroup | null {
  return previewGroup(root, columns);
}

const TRUE_TEXT = new Set(["true", "t", "1", "yes"]);
const FALSE_TEXT = new Set(["false", "f", "0", "no"]);

/**
 * Rewrites conditions whose operator doesn't fit the column's kind when there
 * is an obvious equivalent, such as a foreign-key `eq true` on a boolean
 * column. Returns null when nothing changed.
 */
export function normalizeFilter(root: FilterGroup, columns: Map<string, FilterColumn>): FilterGroup | null {
  let changed = false;
  const visit = (group: FilterGroup): FilterGroup => ({
    ...group,
    children: group.children.map((child) => {
      if (child.kind === "group") {
        return visit(child);
      }
      const column = columns.get(child.column);
      if (column?.kind !== "boolean" || child.operator !== "eq" || child.value.type !== "single") {
        return child;
      }
      const text = scalarText(child.value.value).trim().toLowerCase();
      const operator = TRUE_TEXT.has(text) ? "isTrue" : FALSE_TEXT.has(text) ? "isFalse" : null;
      if (!operator) {
        return child;
      }
      changed = true;
      return { ...child, operator, value: { type: "none" } };
    }),
  });
  const next = visit(root);
  return changed ? next : null;
}
