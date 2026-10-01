import { newId } from "../filters/model";

export const MAX_PANES = 6;
export const MIN_PANE_WIDTH = 320;
export const MIN_PANE_HEIGHT = 220;
export const SPLIT_HANDLE = 6;
export const THREE_PANE_LEFT = 0.55;
/** How close to a pane edge a tab drag splits off a new pane. */
export const DROP_EDGE_ZONE = 28;
/** Share of a pane's width or height, from each edge, that splits the pane when a tab is dropped there. */
export const DROP_EDGE_FRACTION = 0.3;

export type SplitAxis = "x" | "y";
export type SplitDirection = "right" | "down";
export type DropEdge = "left" | "right" | "up" | "down";

export type SplitNode =
  | { type: "leaf"; paneId: string }
  | { type: "split"; axis: SplitAxis; sizes: [number, number]; children: [SplitNode, SplitNode] };

export interface TablePane {
  id: string;
  tabIds: string[];
  activeTabId: string;
}

export interface TableWorkspace {
  layout: SplitNode;
  panes: TablePane[];
  focusedPaneId: string;
  twoPaneAxis: SplitAxis;
}

export function newPaneId() {
  return `pane:${newId("")}`;
}

export function emptyPane(tabIds: string[] = [], activeTabId = ""): TablePane {
  return {
    id: newPaneId(),
    tabIds,
    activeTabId: activeTabId || tabIds[0] || "",
  };
}

export function emptyWorkspace(): TableWorkspace {
  const pane = emptyPane();
  return {
    layout: { type: "leaf", paneId: pane.id },
    panes: [pane],
    focusedPaneId: pane.id,
    twoPaneAxis: "x",
  };
}

export function workspaceFromTabs(tabIds: string[], activeTabId: string): TableWorkspace {
  const pane = emptyPane(tabIds, activeTabId || tabIds[0] || "");
  return {
    layout: { type: "leaf", paneId: pane.id },
    panes: [pane],
    focusedPaneId: pane.id,
    twoPaneAxis: "x",
  };
}

export function cloneNode(node: SplitNode): SplitNode {
  if (node.type === "leaf") {
    return { type: "leaf", paneId: node.paneId };
  }
  return {
    type: "split",
    axis: node.axis,
    sizes: [node.sizes[0], node.sizes[1]],
    children: [cloneNode(node.children[0]), cloneNode(node.children[1])],
  };
}

export function cloneWorkspace(ws: TableWorkspace): TableWorkspace {
  return {
    layout: cloneNode(ws.layout),
    panes: ws.panes.map((pane) => ({ id: pane.id, tabIds: [...pane.tabIds], activeTabId: pane.activeTabId })),
    focusedPaneId: ws.focusedPaneId,
    twoPaneAxis: ws.twoPaneAxis,
  };
}

export function leafIds(node: SplitNode): string[] {
  if (node.type === "leaf") {
    return node.paneId ? [node.paneId] : [];
  }
  return [...leafIds(node.children[0]), ...leafIds(node.children[1])];
}

export function findPane(ws: TableWorkspace, paneId: string): TablePane | undefined {
  return ws.panes.find((pane) => pane.id === paneId);
}

export function paneForTab(ws: TableWorkspace, tabId: string): TablePane | undefined {
  return ws.panes.find((pane) => pane.tabIds.includes(tabId));
}

export function visibleTabIds(ws: TableWorkspace): string[] {
  return ws.panes.map((pane) => pane.activeTabId).filter(Boolean);
}

export function focusedActiveTabId(ws: TableWorkspace): string {
  return findPane(ws, ws.focusedPaneId)?.activeTabId ?? "";
}

export function axisSpan(node: SplitNode, axis: SplitAxis): number {
  if (node.type === "leaf") {
    return 1;
  }
  if (node.axis === axis) {
    return axisSpan(node.children[0], axis) + axisSpan(node.children[1], axis);
  }
  return Math.max(axisSpan(node.children[0], axis), axisSpan(node.children[1], axis));
}

export function nodeAtPath(layout: SplitNode, path: number[]): SplitNode | null {
  let node: SplitNode = layout;
  for (const index of path) {
    if (node.type === "leaf" || (index !== 0 && index !== 1)) {
      return null;
    }
    node = node.children[index];
  }
  return node;
}

function clampPair(sizes: [number, number]): [number, number] {
  const first = Math.min(0.85, Math.max(0.15, sizes[0] / (sizes[0] + sizes[1] || 1)));
  return [first, 1 - first];
}

export function defaultSizesFor(node: SplitNode): [number, number] | null {
  if (node.type !== "split") {
    return null;
  }
  if (node.axis === "x" && node.children[0].type === "leaf" && node.children[1].type === "split") {
    return [THREE_PANE_LEFT, 1 - THREE_PANE_LEFT];
  }
  if (node.axis === "x" && node.children[0].type === "split" && node.children[0].axis === "x") {
    return [2 / 3, 1 / 3];
  }
  return [0.5, 0.5];
}

function pair(axis: SplitAxis, first: string, second: string, left = 0.5): SplitNode {
  return {
    type: "split",
    axis,
    sizes: [left, 1 - left],
    children: [
      { type: "leaf", paneId: first },
      { type: "leaf", paneId: second },
    ],
  };
}

export function buildLayout(paneIds: string[], twoPaneAxis: SplitAxis): SplitNode {
  if (paneIds.length <= 1) {
    return { type: "leaf", paneId: paneIds[0] ?? "" };
  }
  if (paneIds.length === 2) {
    return pair(twoPaneAxis, paneIds[0], paneIds[1]);
  }
  if (paneIds.length === 3) {
    return {
      type: "split",
      axis: "x",
      sizes: [THREE_PANE_LEFT, 1 - THREE_PANE_LEFT],
      children: [{ type: "leaf", paneId: paneIds[0] }, pair("y", paneIds[1], paneIds[2])],
    };
  }
  const twoByTwo: SplitNode = {
    type: "split",
    axis: "x",
    sizes: [0.5, 0.5],
    children: [pair("y", paneIds[0], paneIds[1]), pair("y", paneIds[2], paneIds[3])],
  };
  if (paneIds.length === 4) {
    return twoByTwo;
  }
  if (paneIds.length === 5) {
    return {
      type: "split",
      axis: "x",
      sizes: [2 / 3, 1 / 3],
      children: [twoByTwo, { type: "leaf", paneId: paneIds[4] }],
    };
  }
  return {
    type: "split",
    axis: "x",
    sizes: [2 / 3, 1 / 3],
    children: [twoByTwo, pair("y", paneIds[4], paneIds[5])],
  };
}

function orderedPaneIds(ws: TableWorkspace): string[] {
  const known = new Set(ws.panes.map((pane) => pane.id));
  const order = leafIds(ws.layout).filter((id) => known.has(id));
  for (const pane of ws.panes) {
    if (!order.includes(pane.id)) {
      order.push(pane.id);
    }
  }
  return order;
}

export function reflow(ws: TableWorkspace): TableWorkspace {
  const next = cloneWorkspace(ws);
  const order = orderedPaneIds(next);
  next.layout = buildLayout(order, next.twoPaneAxis);
  if (!next.panes.some((pane) => pane.id === next.focusedPaneId)) {
    next.focusedPaneId = next.panes[0]?.id ?? "";
  }
  return next;
}

export function splitFocused(
  ws: TableWorkspace,
  newPane: TablePane,
  direction: SplitDirection,
): TableWorkspace | { error: "max" | "not-found" } {
  if (ws.panes.length >= MAX_PANES) {
    return { error: "max" };
  }
  if (!findPane(ws, ws.focusedPaneId)) {
    return { error: "not-found" };
  }
  const next = cloneWorkspace(ws);
  next.panes.push(newPane);
  const order = orderedPaneIds(next).filter((id) => id !== newPane.id);
  const index = order.indexOf(ws.focusedPaneId);
  order.splice(index >= 0 ? index + 1 : order.length, 0, newPane.id);
  if (next.panes.length === 2) {
    next.twoPaneAxis = direction === "down" ? "y" : "x";
  }
  next.layout = buildLayout(order, next.twoPaneAxis);
  next.focusedPaneId = newPane.id;
  return next;
}

function replaceLeaf(node: SplitNode, paneId: string, replacement: SplitNode): SplitNode {
  if (node.type === "leaf") {
    return node.paneId === paneId ? replacement : node;
  }
  return {
    ...node,
    children: [replaceLeaf(node.children[0], paneId, replacement), replaceLeaf(node.children[1], paneId, replacement)],
  };
}

/** Panes side by side in one run of same-axis splits, counting a differently split child as one. */
function runLength(node: SplitNode, axis: SplitAxis): number {
  return node.type === "split" && node.axis === axis ? runLength(node.children[0], axis) + runLength(node.children[1], axis) : 1;
}

/** Gives every member of the same-axis run rooted at `node` an equal share. */
function balanceRun(node: SplitNode, axis: SplitAxis): SplitNode {
  if (node.type === "leaf" || node.axis !== axis) {
    return node;
  }
  const first = runLength(node.children[0], axis);
  const second = runLength(node.children[1], axis);
  return {
    ...node,
    sizes: [first / (first + second), second / (first + second)],
    children: [balanceRun(node.children[0], axis), balanceRun(node.children[1], axis)],
  };
}

function runContains(node: SplitNode, axis: SplitAxis, member: SplitNode): boolean {
  if (node === member) {
    return true;
  }
  return node.type === "split" && node.axis === axis && (runContains(node.children[0], axis, member) || runContains(node.children[1], axis, member));
}

/** Evens out the run of `axis` splits that `member` sits in, leaving other sizes alone. */
function balanceAround(node: SplitNode, axis: SplitAxis, member: SplitNode): SplitNode {
  if (node.type === "leaf") {
    return node;
  }
  if (node.axis === axis && runContains(node, axis, member)) {
    return balanceRun(node, axis);
  }
  return {
    ...node,
    children: [balanceAround(node.children[0], axis, member), balanceAround(node.children[1], axis, member)],
  };
}

/** The layout without `paneId`, or null if nothing is left. The sibling takes its place and its run is evened out. */
function removeLeaf(layout: SplitNode, paneId: string): SplitNode | null {
  let collapsed: { sibling: SplitNode; axis: SplitAxis } | null = null;
  const remove = (node: SplitNode): SplitNode | null => {
    if (node.type === "leaf") {
      return node.paneId === paneId ? null : node;
    }
    const first = remove(node.children[0]);
    const second = remove(node.children[1]);
    if (!first || !second) {
      const sibling = first ?? second;
      if (sibling) {
        collapsed = { sibling, axis: node.axis };
      }
      return sibling;
    }
    return { ...node, children: [first, second] };
  };
  const next = remove(layout);
  const done = collapsed as { sibling: SplitNode; axis: SplitAxis } | null;
  return next && done ? balanceAround(next, done.axis, done.sibling) : next;
}

/** Panes across, and down, are capped to keep each one usable. */
export const MAX_PANES_ACROSS = 3;
export const MAX_PANES_DOWN = 2;

function splitLayout(layout: SplitNode, targetId: string, newId: string, edge: DropEdge): SplitNode {
  const axis: SplitAxis = edge === "left" || edge === "right" ? "x" : "y";
  const target: SplitNode = { type: "leaf", paneId: targetId };
  const added: SplitNode = { type: "leaf", paneId: newId };
  const children: [SplitNode, SplitNode] = edge === "left" || edge === "up" ? [added, target] : [target, added];
  return balanceAround(replaceLeaf(layout, targetId, { type: "split", axis, sizes: [0.5, 0.5], children }), axis, added);
}

/** Whether `targetId` can be split on `edge` within the pane limits and a host of this size. */
export function checkSplitPaneAt(
  ws: TableWorkspace,
  targetId: string,
  edge: DropEdge,
  width: number,
  height: number,
): { ok: true } | { ok: false; reason: "max" | "span" | "size" } {
  if (ws.panes.length >= MAX_PANES) {
    return { ok: false, reason: "max" };
  }
  const layout = splitLayout(ws.layout, targetId, "\u0000new", edge);
  const across = axisSpan(layout, "x");
  const down = axisSpan(layout, "y");
  if (across > MAX_PANES_ACROSS || down > MAX_PANES_DOWN) {
    return { ok: false, reason: "span" };
  }
  if (width < across * MIN_PANE_WIDTH + (across - 1) * SPLIT_HANDLE || height < down * MIN_PANE_HEIGHT + (down - 1) * SPLIT_HANDLE) {
    return { ok: false, reason: "size" };
  }
  return { ok: true };
}

/**
 * Splits the pane `targetId` and puts `newPane` on its `edge` side. Panes in the
 * same row or column share it evenly, and every other pane stays where it is.
 */
export function splitPaneAt(
  ws: TableWorkspace,
  targetId: string,
  newPane: TablePane,
  edge: DropEdge,
): TableWorkspace | { error: "max" | "not-found" } {
  if (ws.panes.length >= MAX_PANES) {
    return { error: "max" };
  }
  if (!findPane(ws, targetId)) {
    return { error: "not-found" };
  }
  const next = cloneWorkspace(ws);
  next.panes.push(newPane);
  next.layout = splitLayout(next.layout, targetId, newPane.id, edge);
  if (next.panes.length === 2) {
    next.twoPaneAxis = edge === "left" || edge === "right" ? "x" : "y";
  }
  next.focusedPaneId = newPane.id;
  return next;
}

export function removePane(ws: TableWorkspace, paneId: string, mergeTabs = true): TableWorkspace {
  if (ws.panes.length <= 1) {
    return ws;
  }
  const next = cloneWorkspace(ws);
  const order = orderedPaneIds(next);
  const index = order.indexOf(paneId);
  const neighborId = (index > 0 ? order[index - 1] : order[index + 1]) ?? "";
  const closing = next.panes.find((pane) => pane.id === paneId);
  const neighbor = next.panes.find((pane) => pane.id === neighborId);
  if (mergeTabs && closing && neighbor) {
    neighbor.tabIds = [...neighbor.tabIds, ...closing.tabIds.filter((id) => !neighbor.tabIds.includes(id))];
    if (!neighbor.activeTabId) {
      neighbor.activeTabId = closing.activeTabId;
    }
  }
  next.panes = next.panes.filter((pane) => pane.id !== paneId);
  next.layout =
    removeLeaf(next.layout, paneId) ??
    buildLayout(
      order.filter((id) => id !== paneId),
      next.twoPaneAxis,
    );
  next.focusedPaneId =
    neighbor && next.panes.some((pane) => pane.id === neighbor.id) ? neighbor.id : (next.panes[0]?.id ?? "");
  return next;
}

export function removeTabFromWorkspace(ws: TableWorkspace, tabId: string): TableWorkspace {
  const next = cloneWorkspace(ws);
  const pane = next.panes.find((item) => item.tabIds.includes(tabId));
  if (!pane) {
    return next;
  }
  const index = pane.tabIds.indexOf(tabId);
  pane.tabIds = pane.tabIds.filter((id) => id !== tabId);
  if (pane.activeTabId === tabId) {
    pane.activeTabId = pane.tabIds[Math.min(index, pane.tabIds.length - 1)] ?? "";
  }
  if (!pane.tabIds.length && next.panes.length > 1) {
    return removePane(next, pane.id, false);
  }
  return next;
}

export function addTabToPane(
  ws: TableWorkspace,
  paneId: string,
  tabId: string,
  afterId?: string,
  atStart = false,
): TableWorkspace {
  let next = cloneWorkspace(ws);
  for (const pane of next.panes) {
    if (!pane.tabIds.includes(tabId) || pane.id === paneId) {
      continue;
    }
    pane.tabIds = pane.tabIds.filter((id) => id !== tabId);
    if (pane.activeTabId === tabId) {
      pane.activeTabId = pane.tabIds[0] ?? "";
    }
  }
  const pane = next.panes.find((item) => item.id === paneId);
  if (!pane) {
    return next;
  }
  if (pane.tabIds.includes(tabId)) {
    pane.tabIds = pane.tabIds.filter((id) => id !== tabId);
  }
  const index = afterId ? pane.tabIds.indexOf(afterId) : -1;
  const insertAt = index >= 0 ? index + 1 : atStart ? 0 : pane.tabIds.length;
  pane.tabIds.splice(insertAt, 0, tabId);
  pane.activeTabId = tabId;
  next.focusedPaneId = paneId;
  for (const empty of next.panes.filter((item) => !item.tabIds.length && item.id !== paneId)) {
    if (next.panes.length > 1) {
      next = removePane(next, empty.id, false);
    }
  }
  return next;
}

export function moveTab(
  ws: TableWorkspace,
  tabId: string,
  toPaneId: string,
  afterId?: string,
  atStart = false,
): TableWorkspace {
  return addTabToPane(ws, toPaneId, tabId, afterId, atStart);
}

/** Gap before `midpoints[i]`, or `midpoints.length` to land after the last tab. */
export function insertionIndex(midpoints: number[], x: number): number {
  for (let index = 0; index < midpoints.length; index++) {
    if (x < midpoints[index]) {
      return index;
    }
  }
  return midpoints.length;
}

/**
 * Where `tabId` lands when dropped at `insertAt` in a strip that still lists `tabIds`.
 * `null` means that gap is where the tab already sits.
 */
export function tabStripDrop(
  tabIds: string[],
  tabId: string,
  insertAt: number,
): { atStart: true } | { afterId: string } | null {
  const from = tabIds.indexOf(tabId);
  if (from >= 0 && (insertAt === from || insertAt === from + 1)) {
    return null;
  }
  if (insertAt <= 0) {
    return { atStart: true };
  }
  const before = tabIds[Math.min(insertAt, tabIds.length) - 1];
  if (!before || before === tabId) {
    return null;
  }
  return { afterId: before };
}

export function mergeAllPanes(ws: TableWorkspace, keepActiveTabId: string): TableWorkspace {
  const seen = new Set<string>();
  const tabIds = orderedPaneIds(ws).flatMap((id) =>
    (findPane(ws, id)?.tabIds ?? []).filter((tabId) => {
      if (seen.has(tabId)) {
        return false;
      }
      seen.add(tabId);
      return true;
    }),
  );
  return workspaceFromTabs(tabIds, keepActiveTabId || tabIds[0] || "");
}

export function focusPane(ws: TableWorkspace, paneId: string): TableWorkspace {
  if (ws.focusedPaneId === paneId || !ws.panes.some((pane) => pane.id === paneId)) {
    return ws;
  }
  return { ...ws, focusedPaneId: paneId };
}

export function setPaneActiveTab(ws: TableWorkspace, paneId: string, tabId: string): TableWorkspace {
  const next = cloneWorkspace(ws);
  const pane = next.panes.find((item) => item.id === paneId);
  if (!pane || !pane.tabIds.includes(tabId)) {
    return ws;
  }
  pane.activeTabId = tabId;
  next.focusedPaneId = paneId;
  return next;
}

export function activateTab(ws: TableWorkspace, tabId: string): TableWorkspace {
  const pane = paneForTab(ws, tabId);
  return pane ? setPaneActiveTab(ws, pane.id, tabId) : ws;
}

export function nextSplitDirection(ws: TableWorkspace): SplitDirection | null {
  if (ws.panes.length >= MAX_PANES) {
    return null;
  }
  if (ws.panes.length === 1) {
    return "right";
  }
  if (ws.panes.length === 2) {
    return ws.twoPaneAxis === "x" ? "down" : "right";
  }
  return "right";
}

export function updateSplitSizes(layout: SplitNode, path: number[], sizes: [number, number]): SplitNode {
  if (layout.type === "leaf") {
    return layout;
  }
  if (!path.length) {
    return { ...layout, sizes: clampPair(sizes) };
  }
  const [head, ...rest] = path;
  const children: [SplitNode, SplitNode] = [layout.children[0], layout.children[1]];
  if (head === 0 || head === 1) {
    children[head] = updateSplitSizes(children[head], rest, sizes);
  }
  return { ...layout, children };
}

export function clampDragSizes(
  axis: SplitAxis,
  proposed: number,
  container: number,
  leftSpan: number,
  rightSpan: number,
): [number, number] {
  const min = axis === "x" ? MIN_PANE_WIDTH : MIN_PANE_HEIGHT;
  const available = Math.max(1, container - SPLIT_HANDLE);
  const minLeft = min * leftSpan;
  const minRight = min * rightSpan;
  const minFrac = minLeft / available;
  const maxFrac = 1 - minRight / available;
  if (minFrac >= maxFrac) {
    const left = minLeft / (minLeft + minRight || 1);
    return [left, 1 - left];
  }
  const first = Math.min(maxFrac, Math.max(minFrac, proposed));
  return [first, 1 - first];
}

export function canSplit(
  width: number,
  height: number,
  count: number,
  direction: SplitDirection,
): { ok: true } | { ok: false; reason: "max" | "size" } {
  if (count >= MAX_PANES) {
    return { ok: false, reason: "max" };
  }
  const needW = MIN_PANE_WIDTH * 2 + SPLIT_HANDLE;
  const needW3 = MIN_PANE_WIDTH * 3 + SPLIT_HANDLE * 2;
  const needH = MIN_PANE_HEIGHT * 2 + SPLIT_HANDLE;
  if (count <= 1) {
    if (direction === "right" && width < needW) {
      return { ok: false, reason: "size" };
    }
    if (direction === "down" && height < needH) {
      return { ok: false, reason: "size" };
    }
    return { ok: true };
  }
  if (count >= 4 && width < needW3) {
    return { ok: false, reason: "size" };
  }
  if (width < needW || height < needH) {
    return { ok: false, reason: "size" };
  }
  return { ok: true };
}

/**
 * The allowed edge whose zone the point is deepest in, or null in the middle of the pane.
 * Distances are scaled by each zone's size, so a corner splits along its diagonal.
 */
export function dropEdge(
  rect: DOMRect,
  x: number,
  y: number,
  edges: DropEdge[] = ["left", "right", "up", "down"],
): DropEdge | null {
  const zoneX = Math.max(DROP_EDGE_ZONE, rect.width * DROP_EDGE_FRACTION);
  const zoneY = Math.max(DROP_EDGE_ZONE, rect.height * DROP_EDGE_FRACTION);
  const depth: Record<DropEdge, number> = {
    left: (x - rect.left) / zoneX,
    right: (rect.right - x) / zoneX,
    up: (y - rect.top) / zoneY,
    down: (rect.bottom - y) / zoneY,
  };
  let best: DropEdge | null = null;
  for (const edge of edges) {
    if (depth[edge] < 1 && (!best || depth[edge] < depth[best])) {
      best = edge;
    }
  }
  return best;
}

function restoreLayout(value: unknown, validPanes: Set<string>): SplitNode | null {
  if (!value || typeof value !== "object") {
    return null;
  }
  const node = value as Record<string, unknown>;
  if (node.type === "leaf" && typeof node.paneId === "string" && validPanes.has(node.paneId)) {
    return { type: "leaf", paneId: node.paneId };
  }
  if (
    node.type === "split" &&
    (node.axis === "x" || node.axis === "y") &&
    Array.isArray(node.children) &&
    node.children.length === 2
  ) {
    const left = restoreLayout(node.children[0], validPanes);
    const right = restoreLayout(node.children[1], validPanes);
    if (!left || !right) {
      return left ?? right;
    }
    const raw = Array.isArray(node.sizes) ? node.sizes : [];
    const sizes =
      typeof raw[0] === "number" && typeof raw[1] === "number" ? clampPair([raw[0], raw[1]]) : ([0.5, 0.5] as [number, number]);
    return { type: "split", axis: node.axis, sizes, children: [left, right] };
  }
  return null;
}

function layoutCovers(layout: SplitNode, paneIds: string[]): boolean {
  const leaves = leafIds(layout);
  if (leaves.length !== paneIds.length) {
    return false;
  }
  const remaining = new Set(paneIds);
  return leaves.every((id) => remaining.delete(id)) && remaining.size === 0;
}

export function restoreWorkspace(saved: unknown, tabIds: string[], activeTabId: string): TableWorkspace {
  const fallback = workspaceFromTabs(tabIds, activeTabId);
  if (!saved || typeof saved !== "object") {
    return fallback;
  }
  const item = saved as Record<string, unknown>;
  const validTabs = new Set(tabIds);
  const panes: TablePane[] = [];
  const seen = new Set<string>();
  for (const raw of Array.isArray(item.panes) ? item.panes : []) {
    if (!raw || typeof raw !== "object") {
      continue;
    }
    const pane = raw as Record<string, unknown>;
    if (typeof pane.id !== "string") {
      continue;
    }
    const ids = Array.isArray(pane.tabIds)
      ? pane.tabIds.filter((id): id is string => typeof id === "string" && validTabs.has(id) && !seen.has(id))
      : [];
    for (const id of ids) {
      seen.add(id);
    }
    if (!ids.length && tabIds.length) {
      continue;
    }
    const active =
      typeof pane.activeTabId === "string" && ids.includes(pane.activeTabId) ? pane.activeTabId : (ids[0] ?? "");
    panes.push({ id: pane.id, tabIds: ids, activeTabId: active });
  }
  const missing = tabIds.filter((id) => !seen.has(id));
  if (missing.length) {
    if (!panes.length) {
      return fallback;
    }
    panes[0].tabIds = [...panes[0].tabIds, ...missing];
    if (!panes[0].activeTabId) {
      panes[0].activeTabId = activeTabId || missing[0];
    }
  }
  if (!panes.length) {
    return fallback;
  }
  const twoPaneAxis: SplitAxis = item.twoPaneAxis === "y" ? "y" : "x";
  const restored = restoreLayout(
    item.layout,
    new Set(panes.map((pane) => pane.id)),
  );
  const layout =
    restored && layoutCovers(restored, panes.map((pane) => pane.id))
      ? restored
      : buildLayout(panes.map((pane) => pane.id), twoPaneAxis);
  const focusedFromTab = panes.find((pane) => pane.tabIds.includes(activeTabId))?.id;
  const focused =
    typeof item.focusedPaneId === "string" && panes.some((pane) => pane.id === item.focusedPaneId)
      ? item.focusedPaneId
      : (focusedFromTab ?? panes[0].id);
  return { layout, panes, focusedPaneId: focused, twoPaneAxis };
}
