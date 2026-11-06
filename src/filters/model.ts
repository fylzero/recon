import type { CellEdit, SortDirection } from "../types";

export type FilterKind =
  | "text"
  | "number"
  | "date"
  | "datetime"
  | "time"
  | "boolean"
  | "enum"
  | "uuid"
  | "json"
  | "binary"
  | "other";

export type FilterOperator =
  | "eq"
  | "neq"
  | "contains"
  | "notContains"
  | "startsWith"
  | "endsWith"
  | "gt"
  | "gte"
  | "lt"
  | "lte"
  | "between"
  | "notBetween"
  | "in"
  | "notIn"
  | "isNull"
  | "isNotNull"
  | "isTrue"
  | "isFalse"
  | "isEmpty"
  | "isNotEmpty"
  | "inLast"
  | "inPeriod";

export type RelativeAnchor = "today" | "yesterday" | "tomorrow";

export type DateInput =
  | { kind: "absolute"; value: string }
  | { kind: "relative"; anchor: RelativeAnchor };

/** Text as the user typed it, or a date that may be relative. */
export type FilterScalar = string | DateInput;

export type DateUnit = "day" | "week" | "month" | "year";

export type DatePeriod = "thisWeek" | "lastWeek" | "thisMonth" | "lastMonth" | "thisYear" | "lastYear";

export type FilterValue =
  | { type: "none" }
  | { type: "single"; value: FilterScalar }
  | { type: "range"; from: FilterScalar; to: FilterScalar }
  | { type: "list"; values: string[] }
  | { type: "lastN"; amount: number; unit: DateUnit }
  | { type: "period"; period: DatePeriod };

export interface FilterCondition {
  kind: "condition";
  id: string;
  column: string;
  operator: FilterOperator;
  value: FilterValue;
  enabled: boolean;
}

export type MatchMode = "all" | "any";

export interface FilterGroup {
  kind: "group";
  id: string;
  match: MatchMode;
  children: FilterNode[];
  enabled: boolean;
}

export type FilterNode = FilterCondition | FilterGroup;

export interface TableSort {
  column: string;
  dir: SortDirection;
}

/** Everything that makes one table tab a distinct view, independent of other tabs on the same table. */
export interface TableViewState {
  namespace: string;
  table: string;
  tableKind: "table" | "view";
  filter: FilterGroup;
  sort: TableSort | null;
  panelOpen: boolean;
  /** `link` tabs were opened by a foreign-key arrow and their filter was never edited. */
  origin: "user" | "link";
  title?: string;
  /** Rows this tab loads per page. Missing means the app default, capped at `MAX_TAB_PAGE_SIZE`. */
  pageSize?: number;
  /** Missing or omitted means auto refresh is off. */
  autoRefresh?: AutoRefreshState;
}

export interface AutoRefreshState {
  intervalMs: number;
}

export const AUTO_REFRESH_PRESETS = [
  { label: "5s", ms: 5_000 },
  { label: "30s", ms: 30_000 },
  { label: "1m", ms: 60_000 },
  { label: "5m", ms: 300_000 },
] as const;

export const MIN_AUTO_REFRESH_MS = 1_000;
export const MAX_AUTO_REFRESH_MS = 3_600_000;

export function clampAutoRefreshMs(value: number) {
  if (!Number.isFinite(value)) {
    return AUTO_REFRESH_PRESETS[1].ms;
  }
  return Math.min(MAX_AUTO_REFRESH_MS, Math.max(MIN_AUTO_REFRESH_MS, Math.round(value)));
}

export function formatAutoRefresh(ms: number) {
  const total = Math.max(1, Math.round(ms / 1000));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  if (minutes === 0) {
    return `${seconds}s`;
  }
  return seconds === 0 ? `${minutes}m` : `${minutes}m ${seconds}s`;
}

export function formatCountdown(ms: number) {
  const total = Math.max(0, Math.ceil(ms / 1000));
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  if (minutes === 0) {
    return `${seconds}s`;
  }
  return seconds === 0 ? `${minutes}m` : `${minutes}m ${seconds}s`;
}

export function sanitizeAutoRefresh(value: unknown): AutoRefreshState | undefined {
  if (!value || typeof value !== "object" || (value as { paused?: unknown }).paused === true) {
    return undefined;
  }
  const intervalMs = Number((value as { intervalMs?: unknown }).intervalMs);
  if (!Number.isFinite(intervalMs)) {
    return undefined;
  }
  return { intervalMs: clampAutoRefreshMs(intervalMs) };
}

export const TAB_PAGE_SIZES = [50, 100, 200, 300, 500] as const;
export const MIN_TAB_PAGE_SIZE = 50;
export const MAX_TAB_PAGE_SIZE = 500;

export function clampTabPageSize(value: number) {
  if (!Number.isFinite(value)) {
    return MIN_TAB_PAGE_SIZE;
  }
  return Math.min(MAX_TAB_PAGE_SIZE, Math.max(MIN_TAB_PAGE_SIZE, Math.round(value)));
}

export const MAX_CONDITIONS = 50;
export const MAX_LIST_VALUES = 1000;

export function newId(prefix: string) {
  return `${prefix}${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
}

export function emptyGroup(match: MatchMode = "all"): FilterGroup {
  return { kind: "group", id: newId("g"), match, children: [], enabled: true };
}

export function newCondition(column = "", operator: FilterOperator = "eq", value?: FilterValue): FilterCondition {
  return {
    kind: "condition",
    id: newId("c"),
    column,
    operator,
    value: value ?? { type: "single", value: "" },
    enabled: true,
  };
}

export function conditions(node: FilterNode): FilterCondition[] {
  return node.kind === "condition" ? [node] : node.children.flatMap(conditions);
}

export function hasConditions(root: FilterGroup) {
  return conditions(root).some((condition) => condition.column);
}

export function findNode(root: FilterGroup, id: string): FilterNode | null {
  if (root.id === id) {
    return root;
  }
  for (const child of root.children) {
    if (child.id === id) {
      return child;
    }
    if (child.kind === "group") {
      const found = findNode(child, id);
      if (found) {
        return found;
      }
    }
  }
  return null;
}

/** Replaces the node with `id` by what `update` returns, or removes it when that is null. */
export function updateNode(
  root: FilterGroup,
  id: string,
  update: (node: FilterNode) => FilterNode | null,
): FilterGroup {
  const visit = (group: FilterGroup): FilterGroup => ({
    ...group,
    children: group.children.flatMap((child) => {
      if (child.id === id) {
        const next = update(child);
        return next ? [next] : [];
      }
      return child.kind === "group" ? [visit(child)] : [child];
    }),
  });
  if (root.id === id) {
    const next = update(root);
    return next?.kind === "group" ? next : { ...root, children: [] };
  }
  return pruneEmptyGroups(visit(root));
}

function pruneEmptyGroups(root: FilterGroup): FilterGroup {
  return {
    ...root,
    children: root.children.flatMap<FilterNode>((child) => {
      if (child.kind === "condition") {
        return [child];
      }
      const pruned = pruneEmptyGroups(child);
      return pruned.children.length ? [pruned] : [];
    }),
  };
}

export function removeNode(root: FilterGroup, id: string) {
  return updateNode(root, id, () => null);
}

export function appendChild(root: FilterGroup, groupId: string, node: FilterNode): FilterGroup {
  return updateNode(root, groupId, (group) =>
    group.kind === "group" ? { ...group, children: [...group.children, node] } : group,
  );
}

export function insertAfter(root: FilterGroup, id: string, node: FilterNode): FilterGroup {
  const visit = (group: FilterGroup): FilterGroup => ({
    ...group,
    children: group.children.flatMap((child) => {
      if (child.id === id) {
        return [child, node];
      }
      return child.kind === "group" ? [visit(child)] : [child];
    }),
  });
  return visit(root);
}

export function cloneNode<T extends FilterNode>(node: T): T {
  if (node.kind === "condition") {
    // structuredClone rejects Vue's reactive proxies; filter values are plain JSON.
    return { ...node, id: newId("c"), value: JSON.parse(JSON.stringify(node.value)) as FilterValue };
  }
  return { ...node, id: newId("g"), children: node.children.map(cloneNode) } as T;
}

export function renameColumn(root: FilterGroup, from: string, to: string): FilterGroup {
  const visit = (group: FilterGroup): FilterGroup => ({
    ...group,
    children: group.children.map((child) => {
      if (child.kind === "group") {
        return visit(child);
      }
      return child.column === from ? { ...child, column: to } : child;
    }),
  });
  return visit(root);
}

/** Removes conditions that never got a column, as when the panel collapses. */
export function dropBlankConditions(root: FilterGroup): FilterGroup {
  const visit = (group: FilterGroup): FilterGroup => ({
    ...group,
    children: group.children.flatMap<FilterNode>((child) => {
      if (child.kind === "group") {
        return [visit(child)];
      }
      return child.column ? [child] : [];
    }),
  });
  return pruneEmptyGroups(visit(root));
}

/** The filter a foreign-key arrow opens: every referenced column equals the row's value. */
export function linkFilter(cells: CellEdit[]): FilterGroup {
  const group = emptyGroup("all");
  group.children = cells.map((cell) =>
    cell.value === null
      ? newCondition(cell.column, "isNull", { type: "none" })
      : newCondition(cell.column, "eq", { type: "single", value: String(cell.value) }),
  );
  return group;
}

const OPERATORS = new Set<string>([
  "eq", "neq", "contains", "notContains", "startsWith", "endsWith", "gt", "gte", "lt", "lte",
  "between", "notBetween", "in", "notIn", "isNull", "isNotNull", "isTrue", "isFalse", "isEmpty", "isNotEmpty", "inLast", "inPeriod",
]);
const ANCHORS = new Set<string>(["today", "yesterday", "tomorrow"]);
const UNITS = new Set<string>(["day", "week", "month", "year"]);
const PERIODS = new Set<string>(["thisWeek", "lastWeek", "thisMonth", "lastMonth", "thisYear", "lastYear"]);

function sanitizeScalar(value: unknown): FilterScalar {
  if (typeof value === "string") {
    return value;
  }
  if (value && typeof value === "object") {
    const input = value as Record<string, unknown>;
    if (input.kind === "relative" && typeof input.anchor === "string" && ANCHORS.has(input.anchor)) {
      return { kind: "relative", anchor: input.anchor as RelativeAnchor };
    }
    if (input.kind === "absolute" && typeof input.value === "string") {
      return { kind: "absolute", value: input.value };
    }
  }
  return "";
}

function sanitizeValue(value: unknown): FilterValue {
  const input = (value && typeof value === "object" ? value : {}) as Record<string, unknown>;
  switch (input.type) {
    case "none":
      return { type: "none" };
    case "range":
      return { type: "range", from: sanitizeScalar(input.from), to: sanitizeScalar(input.to) };
    case "list":
      return {
        type: "list",
        values: Array.isArray(input.values) ? input.values.filter((item): item is string => typeof item === "string") : [],
      };
    case "lastN":
      return {
        type: "lastN",
        amount: typeof input.amount === "number" && Number.isFinite(input.amount) ? input.amount : 7,
        unit: typeof input.unit === "string" && UNITS.has(input.unit) ? (input.unit as DateUnit) : "day",
      };
    case "period":
      return {
        type: "period",
        period: typeof input.period === "string" && PERIODS.has(input.period) ? (input.period as DatePeriod) : "thisMonth",
      };
    default:
      return { type: "single", value: sanitizeScalar(input.value) };
  }
}

function sanitizeNode(value: unknown, depth: number): FilterNode | null {
  if (!value || typeof value !== "object" || depth > 4) {
    return null;
  }
  const input = value as Record<string, unknown>;
  const enabled = input.enabled !== false;
  if (input.kind === "group") {
    return {
      kind: "group",
      id: typeof input.id === "string" ? input.id : newId("g"),
      match: input.match === "any" ? "any" : "all",
      enabled,
      children: (Array.isArray(input.children) ? input.children : [])
        .map((child) => sanitizeNode(child, depth + 1))
        .filter((child): child is FilterNode => child !== null),
    };
  }
  if (input.kind === "condition" && typeof input.column === "string") {
    return {
      kind: "condition",
      id: typeof input.id === "string" ? input.id : newId("c"),
      column: input.column,
      operator: typeof input.operator === "string" && OPERATORS.has(input.operator)
        ? (input.operator as FilterOperator)
        : "eq",
      value: sanitizeValue(input.value),
      enabled,
    };
  }
  return null;
}

/** Rebuilds a filter read from storage, dropping anything malformed. */
export function sanitizeFilter(value: unknown): FilterGroup {
  const node = sanitizeNode(value, 0);
  return node?.kind === "group" ? node : emptyGroup();
}

const CLIPBOARD_TAG = "recon.filter";
const CLIPBOARD_VERSION = 1;

type PortableNode =
  | Omit<FilterCondition, "id">
  | (Omit<FilterGroup, "id" | "children"> & { children: PortableNode[] });

function portable(node: FilterNode): PortableNode {
  if (node.kind === "condition") {
    return { kind: "condition", column: node.column, operator: node.operator, value: node.value, enabled: node.enabled };
  }
  return { kind: "group", match: node.match, enabled: node.enabled, children: node.children.map(portable) };
}

/** The text Copy filters puts on the clipboard. Ids are left out so every paste gets its own. */
export function filterToClipboard(root: FilterGroup): string {
  const payload = { format: CLIPBOARD_TAG, version: CLIPBOARD_VERSION, filter: portable(dropBlankConditions(root)) };
  return JSON.stringify(payload, null, 2);
}

/** Reads a filter copied with `filterToClipboard`, or returns null when the text isn't one. */
export function filterFromClipboard(text: string): FilterGroup | null {
  let payload: unknown;
  try {
    payload = JSON.parse(text.trim());
  } catch {
    return null;
  }
  if (!payload || typeof payload !== "object") {
    return null;
  }
  const input = payload as Record<string, unknown>;
  if (input.format !== CLIPBOARD_TAG || typeof input.version !== "number" || input.version > CLIPBOARD_VERSION) {
    return null;
  }
  const filter = input.filter as Record<string, unknown> | null;
  if (!filter || filter.kind !== "group") {
    return null;
  }
  return cloneNode(dropBlankConditions(sanitizeFilter(filter)));
}

/** Children that can sit directly in an AND group without changing what `group` matches. */
function andParts(group: FilterGroup): FilterNode[] {
  return group.enabled && (group.match === "all" || group.children.length < 2) ? group.children : [group];
}

/** Combines two filters so rows must match both, keeping an OR filter intact by nesting it. */
export function mergeFilters(base: FilterGroup, added: FilterGroup): FilterGroup {
  const kept = dropBlankConditions(base);
  if (!kept.children.length) {
    return { ...added, id: kept.id };
  }
  return { ...kept, match: "all", enabled: true, children: [...andParts(kept), ...andParts(added)] };
}
