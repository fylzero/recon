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
  /** Part of a `{name}_type` and `{name}_id` pair. */
  polymorphic: boolean;
  /** For a polymorphic pair, the stored types and the tables they point to, one per line. */
  morphTargets: string;
  /** Where a foreign-key column points, as `table.column` or `schema.table.column`. */
  references: string;
  /** For an `_id` column with no foreign key, the `table.column` its name suggests. */
  suggests: string;
}

/** An `_id` column with no foreign key whose name matches another table. */
export interface MissingKey {
  table: string;
  column: string;
  refTable: string;
  refColumn: string;
}

/** A `{name}_type` and `{name}_id` column pair, like Laravel's `morphs()` creates. */
export interface MorphColumn {
  table: string;
  name: string;
  typeColumn: string;
  idColumn: string;
}

export interface MorphTarget {
  /** The value stored in the type column, like `App\Models\Post` or `post`. */
  type: string;
  /** The table it points to, or empty if it couldn't be matched. */
  table: string;
  manual: boolean;
}

export interface ResolvedMorph extends MorphColumn {
  targets: MorphTarget[];
}

/** Which links to follow. Foreign keys are followed unless `foreign` is false. */
export interface DiagramLinks {
  foreign?: boolean;
  morphs?: ResolvedMorph[];
  missing?: MissingKey[];
}

/** Manual picks keyed by `morphKey`, then by type value. An empty table means not linked. */
export type MorphOverrides = Record<string, Record<string, string>>;

export interface DiagramNode {
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  rows: DiagramRow[];
  hidden: number;
}

export interface DiagramEdge {
  id: string;
  kind: "foreign" | "morph" | "missing";
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

const TYPE_SUFFIX = "_type";
const TEXT_TYPE = /char|text|string|enum/i;
const ID_SUFFIX = /^(.+?)(?:_id|Id)$/;
const IRREGULAR_PLURALS: Record<string, string> = {
  person: "people",
  child: "children",
  man: "men",
  woman: "women",
};

export function morphKey(column: MorphColumn) {
  return `${column.table}.${column.typeColumn}`;
}

/** Text `{name}_type` columns with a `{name}_id` beside them that isn't already a foreign key. */
export function morphColumns(diagram: SchemaDiagram): MorphColumn[] {
  const found: MorphColumn[] = [];
  for (const table of diagram.tables) {
    const names = new Set(table.columns.map((column) => column.name));
    const keyed = new Set(
      diagram.foreignKeys.filter((key) => key.table === table.name).flatMap((key) => key.columns),
    );
    for (const column of table.columns) {
      if (!column.name.endsWith(TYPE_SUFFIX) || !TEXT_TYPE.test(column.dataType)) {
        continue;
      }
      const name = column.name.slice(0, -TYPE_SUFFIX.length);
      const idColumn = `${name}_id`;
      if (name && names.has(idColumn) && !keyed.has(idColumn)) {
        found.push({ table: table.name, name, typeColumn: column.name, idColumn });
      }
    }
  }
  return found;
}

function snakeCase(word: string) {
  return word
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1_$2")
    .replace(/([a-z\d])([A-Z])/g, "$1_$2")
    .replace(/[\s-]+/g, "_")
    .toLowerCase();
}

function pluralize(word: string) {
  const parts = word.split("_");
  const last = parts.pop() ?? "";
  let plural: string;
  if (IRREGULAR_PLURALS[last]) {
    plural = IRREGULAR_PLURALS[last];
  } else if (/[^aeiou]y$/.test(last)) {
    plural = `${last.slice(0, -1)}ies`;
  } else if (/(s|x|z|ch|sh)$/.test(last)) {
    plural = `${last}es`;
  } else {
    plural = `${last}s`;
  }
  return [...parts, plural].join("_");
}

/**
 * Matches a stored type to a table the way Laravel and Rails name them, so
 * `App\Models\BlogPost`, `Admin::BlogPost`, and `blog_post` all find `blog_posts`.
 */
export function guessMorphTable(type: string, tables: Iterable<string>) {
  const lookup = new Map<string, string>();
  for (const table of tables) {
    lookup.set(table.toLowerCase(), table);
  }
  const base = type.split(/\\|::|\//).filter(Boolean).pop() ?? "";
  if (!base) {
    return "";
  }
  const name = snakeCase(base);
  return lookup.get(pluralize(name)) ?? lookup.get(name) ?? "";
}

/**
 * `{name}_id` and `{name}Id` columns that aren't a primary key, foreign key, or
 * polymorphic pair, but whose name matches a table with a single-column primary
 * key. `parent_id` points back to its own table.
 */
export function missingKeys(diagram: SchemaDiagram): MissingKey[] {
  const tables = diagram.tables.map((table) => table.name);
  const primary = new Map<string, string>();
  for (const table of diagram.tables) {
    const keys = table.columns.filter((column) => column.primaryKey);
    if (keys.length === 1) {
      primary.set(table.name, keys[0].name);
    }
  }
  const morphIds = new Set(morphColumns(diagram).map((morph) => `${morph.table}.${morph.idColumn}`));
  const found: MissingKey[] = [];
  for (const table of diagram.tables) {
    const keyed = new Set(
      diagram.foreignKeys.filter((key) => key.table === table.name).flatMap((key) => key.columns),
    );
    for (const column of table.columns) {
      const name = ID_SUFFIX.exec(column.name)?.[1];
      if (!name || column.primaryKey || keyed.has(column.name) || morphIds.has(`${table.name}.${column.name}`)) {
        continue;
      }
      const refTable = snakeCase(name) === "parent" ? table.name : guessMorphTable(name, tables);
      const refColumn = primary.get(refTable);
      if (refColumn) {
        found.push({ table: table.name, column: column.name, refTable, refColumn });
      }
    }
  }
  return found;
}

/** Pairs each morph column with the tables its stored types point to, manual picks first. */
export function resolveMorphs(
  columns: MorphColumn[],
  types: Record<string, string[]>,
  tables: string[],
  overrides: MorphOverrides,
): ResolvedMorph[] {
  const known = new Set(tables);
  return columns.map((column) => {
    const key = morphKey(column);
    const picked = overrides[key] ?? {};
    const values = [...new Set([...(types[key] ?? []), ...Object.keys(picked)])];
    const targets = values.map((type) => {
      if (type in picked) {
        return { type, table: known.has(picked[type]) ? picked[type] : "", manual: true };
      }
      return { type, table: guessMorphTable(type, tables), manual: false };
    });
    return { ...column, targets };
  });
}

/** Each morph column's linked tables, once per table even when several types point to it. */
function morphTargets(morph: ResolvedMorph) {
  const byTable = new Map<string, string[]>();
  for (const target of morph.targets) {
    if (target.table) {
      byTable.set(target.table, [...(byTable.get(target.table) ?? []), target.type]);
    }
  }
  return byTable;
}

/**
 * The selected tables, plus every table they reference or are referenced by
 * when `neighbours` is on, including through polymorphic links. Only tables
 * that exist in the diagram are kept.
 */
export function scopeTables(
  diagram: SchemaDiagram,
  namespace: string,
  selected: Iterable<string>,
  neighbours: boolean,
  links: DiagramLinks = {},
): Set<string> {
  const { foreign = true, morphs = [], missing = [] } = links;
  const known = new Set(diagram.tables.map((table) => table.name));
  const picked = new Set([...selected].filter((name) => known.has(name)));
  if (!neighbours) {
    return picked;
  }
  const scoped = new Set(picked);
  for (const key of foreign ? diagram.foreignKeys : []) {
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
  for (const morph of morphs) {
    for (const table of morphTargets(morph).keys()) {
      if (picked.has(morph.table) && known.has(table)) {
        scoped.add(table);
      }
      if (picked.has(table)) {
        scoped.add(morph.table);
      }
    }
  }
  for (const key of missing) {
    if (picked.has(key.table)) {
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
 * Lays out `tables` left to right by their foreign keys and polymorphic links.
 * Tables with no link to another visible table are packed into a grid underneath,
 * so a schema full of unrelated tables doesn't become one long column.
 */
export function layoutDiagram(
  diagram: SchemaDiagram,
  namespace: string,
  tables: Set<string>,
  options: DiagramLinks & { keysOnly: boolean },
): DiagramLayout {
  const keys = diagram.foreignKeys.filter((key) => tables.has(key.table));
  const morphs = (options.morphs ?? []).filter((morph) => tables.has(morph.table));
  const missing = (options.missing ?? []).filter((key) => tables.has(key.table));
  const morphRows = new Map<string, Map<string, string>>();
  for (const morph of morphs) {
    const byColumn = morphRows.get(morph.table) ?? new Map<string, string>();
    const summary = morph.targets.length
      ? morph.targets.map((target) => `${target.type} → ${target.table || "no table"}`).join("\n")
      : "No stored types yet";
    byColumn.set(morph.typeColumn, summary);
    byColumn.set(morph.idColumn, summary);
    morphRows.set(morph.table, byColumn);
  }
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
  const suggested = new Map<string, Map<string, string>>();
  for (const key of missing) {
    const byColumn = suggested.get(key.table) ?? new Map<string, string>();
    byColumn.set(key.column, `${key.refTable}.${key.refColumn}`);
    suggested.set(key.table, byColumn);
    if (tables.has(key.refTable)) {
      const set = referenced.get(key.refTable) ?? new Set<string>();
      set.add(key.refColumn);
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
    const polymorphic = morphRows.get(table.name);
    const suggests = suggested.get(table.name);
    const all: DiagramRow[] = table.columns.map((column) => ({
      name: column.name,
      dataType: column.dataType,
      primaryKey: column.primaryKey,
      foreignKey: Boolean(outgoing?.has(column.name)),
      polymorphic: Boolean(polymorphic?.has(column.name)),
      morphTargets: polymorphic?.get(column.name) ?? "",
      references: outgoing?.get(column.name) ?? "",
      suggests: suggests?.get(column.name) ?? "",
    }));
    const rows = options.keysOnly
      ? all.filter(
          (row) => row.primaryKey || row.foreignKey || row.polymorphic || row.suggests || incoming?.has(row.name),
        )
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
    });
  }

  const links: (Omit<DiagramEdge, "path"> & { fromColumn: string; toColumn: string })[] = keys
    .filter((key) => options.foreign !== false && isLocal(key, namespace) && nodes.has(key.refTable))
    .map((key) => ({
      id: `${key.table}:${key.name}`,
      kind: "foreign",
      from: key.table,
      to: key.refTable,
      fromColumn: key.columns[0],
      toColumn: key.refColumns[0],
      label: `${key.table}.${key.columns.join(", ")} → ${key.refTable}.${key.refColumns.join(", ")}`,
    }));
  for (const morph of morphs) {
    for (const [table, types] of morphTargets(morph)) {
      const target = nodes.get(table);
      if (!target) {
        continue;
      }
      links.push({
        id: `${morph.table}:morph:${morph.name}:${table}`,
        kind: "morph",
        from: morph.table,
        to: table,
        fromColumn: morph.idColumn,
        toColumn: target.rows.find((row) => row.primaryKey)?.name ?? "",
        label: `${morph.table}.${morph.name} (polymorphic) → ${table} as ${types.join(", ")}`,
      });
    }
  }
  for (const key of missing) {
    if (nodes.has(key.refTable)) {
      links.push({
        id: `${key.table}:missing:${key.column}`,
        kind: "missing",
        from: key.table,
        to: key.refTable,
        fromColumn: key.column,
        toColumn: key.refColumn,
        label: `${key.table}.${key.column} → ${key.refTable}.${key.refColumn} (no foreign key)`,
      });
    }
  }
  const linked = new Set<string>();
  for (const link of links) {
    if (link.from !== link.to) {
      linked.add(link.from);
      linked.add(link.to);
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
    for (const link of links) {
      if (link.from !== link.to) {
        graph.setEdge(link.to, link.from, {}, link.id);
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

  const edges = links.map(({ fromColumn, toColumn, ...link }) => {
    const source = nodes.get(link.from)!;
    const target = nodes.get(link.to)!;
    return { ...link, path: edgePath(source, target, rowCenter(source, fromColumn), rowCenter(target, toColumn)) };
  });

  return { nodes: [...nodes.values()], edges, width, height };
}
