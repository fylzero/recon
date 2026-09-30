import dagre from "@dagrejs/dagre";
import type { DiagramForeignKey, SchemaDiagram } from "../types";

/* These must match the `.diagram-table-name`, `.diagram-row`, and `.diagram-more` heights in styles.css. */
const HEADER_HEIGHT = 30;
const ROW_HEIGHT = 22;
const FOOTER_HEIGHT = 22;
const BORDER = 1;
const CHAR_WIDTH = 7.2;
const MIN_WIDTH = 150;
const MAX_WIDTH = 320;
const GAP = 48;

export interface DiagramRow {
  name: string;
  dataType: string;
  primaryKey: boolean;
  foreignKey: boolean;
  /** Where a foreign-key column points, as `table.column` or `schema.table.column`. */
  references: string;
}

export interface DiagramNode {
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  rows: DiagramRow[];
  hidden: number;
  focus: boolean;
}

export interface DiagramEdge {
  id: string;
  from: string;
  to: string;
  path: string;
  label: string;
}

export interface DiagramLayout {
  nodes: DiagramNode[];
  edges: DiagramEdge[];
  width: number;
  height: number;
}

function isLocal(key: DiagramForeignKey, namespace: string) {
  return !key.refNamespace || key.refNamespace === namespace;
}

/**
 * The selected tables, plus every table they reference or are referenced by
 * when `neighbours` is on. Only tables that exist in the diagram are kept.
 */
export function scopeTables(
  diagram: SchemaDiagram,
  namespace: string,
  selected: Iterable<string>,
  neighbours: boolean,
): Set<string> {
  const known = new Set(diagram.tables.map((table) => table.name));
  const picked = new Set([...selected].filter((name) => known.has(name)));
  if (!neighbours) {
    return picked;
  }
  const scoped = new Set(picked);
  for (const key of diagram.foreignKeys) {
    if (!isLocal(key, namespace)) {
      continue;
    }
    if (picked.has(key.table) && known.has(key.refTable)) {
      scoped.add(key.refTable);
    }
    if (picked.has(key.refTable)) {
      scoped.add(key.table);
    }
  }
  return scoped;
}

function boxWidth(name: string, rows: DiagramRow[]) {
  const longest = Math.max(name.length + 4, ...rows.map((row) => row.name.length + row.dataType.length + 6));
  return Math.round(Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, longest * CHAR_WIDTH + 24)));
}

function rowCenter(node: DiagramNode, column: string) {
  const index = node.rows.findIndex((row) => row.name === column);
  const top = node.y - node.height / 2 + BORDER;
  return index < 0 ? top + HEADER_HEIGHT / 2 : top + HEADER_HEIGHT + index * ROW_HEIGHT + ROW_HEIGHT / 2;
}

function edgePath(source: DiagramNode, target: DiagramNode, sy: number, ty: number) {
  const sourceLeft = source.x - source.width / 2;
  const sourceRight = source.x + source.width / 2;
  const targetLeft = target.x - target.width / 2;
  const targetRight = target.x + target.width / 2;
  if (source === target) {
    const loop = 36;
    return `M ${sourceRight} ${sy} C ${sourceRight + loop} ${sy}, ${sourceRight + loop} ${ty}, ${sourceRight} ${ty}`;
  }
  let sx: number, tx: number, sDir: number, tDir: number;
  if (targetLeft >= sourceRight - 8) {
    [sx, tx, sDir, tDir] = [sourceRight, targetLeft, 1, -1];
  } else if (targetRight <= sourceLeft + 8) {
    [sx, tx, sDir, tDir] = [sourceLeft, targetRight, -1, 1];
  } else {
    [sx, tx, sDir, tDir] = [sourceRight, targetRight, 1, 1];
  }
  const reach = Math.max(40, Math.abs(tx - sx) / 2);
  return `M ${sx} ${sy} C ${sx + sDir * reach} ${sy}, ${tx + tDir * reach} ${ty}, ${tx} ${ty}`;
}

/**
 * Lays out `tables` left to right by their foreign keys. Tables with no link to
 * another visible table are packed into a grid underneath, so a schema full of
 * unrelated tables doesn't become one long column.
 */
export function layoutDiagram(
  diagram: SchemaDiagram,
  namespace: string,
  tables: Set<string>,
  options: { keysOnly: boolean; focus?: Set<string> },
): DiagramLayout {
  const keys = diagram.foreignKeys.filter((key) => tables.has(key.table));
  const fkColumns = new Map<string, Map<string, string>>();
  const referenced = new Map<string, Set<string>>();
  for (const key of keys) {
    const local = isLocal(key, namespace);
    const byColumn = fkColumns.get(key.table) ?? new Map<string, string>();
    key.columns.forEach((column, index) => {
      const target = local ? key.refTable : `${key.refNamespace}.${key.refTable}`;
      byColumn.set(column, `${target}.${key.refColumns[index] ?? ""}`);
    });
    fkColumns.set(key.table, byColumn);
    if (local && tables.has(key.refTable)) {
      const set = referenced.get(key.refTable) ?? new Set<string>();
      key.refColumns.forEach((column) => set.add(column));
      referenced.set(key.refTable, set);
    }
  }

  const nodes = new Map<string, DiagramNode>();
  for (const table of diagram.tables) {
    if (!tables.has(table.name)) {
      continue;
    }
    const outgoing = fkColumns.get(table.name);
    const incoming = referenced.get(table.name);
    const all: DiagramRow[] = table.columns.map((column) => ({
      name: column.name,
      dataType: column.dataType,
      primaryKey: column.primaryKey,
      foreignKey: Boolean(outgoing?.has(column.name)),
      references: outgoing?.get(column.name) ?? "",
    }));
    const rows = options.keysOnly
      ? all.filter((row) => row.primaryKey || row.foreignKey || incoming?.has(row.name))
      : all;
    const hidden = all.length - rows.length;
    nodes.set(table.name, {
      name: table.name,
      x: 0,
      y: 0,
      width: boxWidth(table.name, rows),
      height: HEADER_HEIGHT + rows.length * ROW_HEIGHT + (hidden ? FOOTER_HEIGHT : 0) + BORDER * 2,
      rows,
      hidden,
      focus: Boolean(options.focus?.has(table.name)),
    });
  }

  const links = keys.filter((key) => isLocal(key, namespace) && nodes.has(key.refTable));
  const linked = new Set<string>();
  for (const key of links) {
    if (key.table !== key.refTable) {
      linked.add(key.table);
      linked.add(key.refTable);
    }
  }

  let width = 0;
  let height = 0;
  if (linked.size) {
    const graph = new dagre.graphlib.Graph({ multigraph: true });
    graph.setGraph({ rankdir: "LR", nodesep: 32, ranksep: 96, marginx: 0, marginy: 0 });
    graph.setDefaultEdgeLabel(() => ({}));
    for (const name of linked) {
      const node = nodes.get(name)!;
      graph.setNode(name, { width: node.width, height: node.height });
    }
    for (const key of links) {
      if (key.table !== key.refTable) {
        graph.setEdge(key.refTable, key.table, {}, `${key.table}:${key.name}`);
      }
    }
    dagre.layout(graph);
    for (const name of linked) {
      const placed = graph.node(name);
      const node = nodes.get(name)!;
      node.x = placed.x ?? 0;
      node.y = placed.y ?? 0;
      width = Math.max(width, node.x + node.width / 2);
      height = Math.max(height, node.y + node.height / 2);
    }
  }

  const loose = [...nodes.values()].filter((node) => !linked.has(node.name));
  if (loose.length) {
    const rowWidth = Math.max(width, 1200);
    let x = 0;
    let y = linked.size ? height + GAP * 2 : 0;
    let tallest = 0;
    for (const node of loose) {
      if (x > 0 && x + node.width > rowWidth) {
        x = 0;
        y += tallest + GAP;
        tallest = 0;
      }
      node.x = x + node.width / 2;
      node.y = y + node.height / 2;
      x += node.width + GAP;
      tallest = Math.max(tallest, node.height);
      width = Math.max(width, node.x + node.width / 2);
      height = Math.max(height, y + node.height);
    }
  }

  const edges = links.map((key) => {
    const source = nodes.get(key.table)!;
    const target = nodes.get(key.refTable)!;
    const path = edgePath(source, target, rowCenter(source, key.columns[0]), rowCenter(target, key.refColumns[0]));
    return {
      id: `${key.table}:${key.name}`,
      from: key.table,
      to: key.refTable,
      path,
      label: `${key.table}.${key.columns.join(", ")} → ${key.refTable}.${key.refColumns.join(", ")}`,
    };
  });

  return { nodes: [...nodes.values()], edges, width, height };
}
