import type { Cell, ColumnMeta, MongoDocument, MongoKind } from "./types";

export interface RiskyCommand {
  title: string;
  message: string;
  action: string;
}

/** Console commands that delete a lot at once, so they're confirmed first. */
export function destructiveMongoCommand(line: string, db: string): RiskyCommand | null {
  const text = line.replace(/\s+/g, " ");
  if (/^db\.dropDatabase\(/.test(text) || /^\{ ?["']?dropDatabase["']? ?:/.test(text)) {
    return {
      title: "Drop database?",
      message: `This deletes the database ${db} and everything in it. This can't be undone.`,
      action: "Drop database",
    };
  }
  const collection = /^db\.(?:getCollection\(["']([^"']+)["']\)|([\w$.-]+))\.drop\( ?\)/.exec(text);
  if (collection || /^\{ ?["']?drop["']? ?:/.test(text)) {
    const name = collection?.[1] ?? collection?.[2] ?? "this collection";
    return {
      title: "Drop collection?",
      message: `This deletes ${name} with all of its documents and indexes. This can't be undone.`,
      action: "Drop collection",
    };
  }
  if (/\.(deleteMany|remove)\( ?(\{ ?\})? ?\)/.test(text)) {
    return {
      title: "Delete every document?",
      message: "This deletes every document in the collection. This can't be undone.",
      action: "Delete all",
    };
  }
  return null;
}

const KIND_LABELS: Partial<Record<MongoKind, string>> = {
  string: "string",
  int: "int",
  long: "long",
  double: "double",
  decimal: "decimal",
  bool: "bool",
  null: "null",
  objectId: "ObjectId",
  date: "date",
  object: "object",
  array: "array",
  binary: "binary",
  regex: "regex",
  timestamp: "timestamp",
};

/** Kinds the grid can edit in place. Others are edited as text. */
const EDITABLE: Set<MongoKind> = new Set([
  "string",
  "int",
  "long",
  "double",
  "decimal",
  "bool",
  "null",
  "objectId",
  "date",
]);

export interface DocumentGrid {
  columns: ColumnMeta[];
  rows: Cell[][];
  /** Each cell's kind, or undefined where the document has no such field. */
  kinds: (MongoKind | undefined)[][];
}

/** Columns from every field in the loaded documents, `_id` first, in the order they appear. */
export function documentGrid(documents: MongoDocument[]): DocumentGrid {
  const order: string[] = [];
  const seen = new Map<string, Set<MongoKind>>();
  for (const doc of documents) {
    for (const field of doc.fields) {
      let kinds = seen.get(field.name);
      if (!kinds) {
        kinds = new Set();
        seen.set(field.name, kinds);
        order.push(field.name);
      }
      kinds.add(field.kind);
    }
  }
  const idAt = order.indexOf("_id");
  if (idAt > 0) {
    order.splice(idAt, 1);
    order.unshift("_id");
  }
  const columns = order.map((name) => {
    const kinds = [...(seen.get(name) ?? [])].filter((kind) => kind !== "null");
    const typeName = kinds.length === 1 ? (KIND_LABELS[kinds[0]] ?? kinds[0]) : kinds.length ? "mixed" : "null";
    return { name, typeName };
  });
  const index = new Map(order.map((name, at) => [name, at]));
  const rows: Cell[][] = [];
  const kinds: (MongoKind | undefined)[][] = [];
  for (const doc of documents) {
    const row: Cell[] = order.map(() => "");
    const rowKinds: (MongoKind | undefined)[] = order.map(() => undefined);
    for (const field of doc.fields) {
      const at = index.get(field.name)!;
      row[at] = field.value;
      rowKinds[at] = field.kind;
    }
    rows.push(row);
    kinds.push(rowKinds);
  }
  return { columns, rows, kinds };
}

export function cellEditable(kind: MongoKind | undefined, field: string) {
  return field !== "_id" && !field.includes(".") && !field.startsWith("$") && (kind === undefined || EDITABLE.has(kind));
}

export function formatCount(count: number | null) {
  if (count === null) return "";
  if (count < 1000) return String(count);
  if (count < 1_000_000) return `${(count / 1000).toFixed(count < 10_000 ? 1 : 0)}K`;
  return `${(count / 1_000_000).toFixed(count < 10_000_000 ? 1 : 0)}M`;
}

/** A short label for an `_id`, for messages. */
export function idLabel(id: unknown): string {
  if (id && typeof id === "object" && "$oid" in id) return String((id as { $oid: string }).$oid);
  return typeof id === "string" ? id : JSON.stringify(id);
}
