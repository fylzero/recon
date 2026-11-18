import type { RedisBytes, RedisKeyEntry, RedisReply } from "./types";

export const REDIS_KINDS = ["string", "hash", "list", "set", "zset", "stream"] as const;

export const KIND_LABELS: Record<string, string> = {
  string: "String",
  hash: "Hash",
  list: "List",
  set: "Set",
  zset: "Sorted set",
  stream: "Stream",
};

/** Short tags for the key list, colored per type in the stylesheet. */
export const KIND_TAGS: Record<string, string> = {
  string: "STR",
  hash: "HASH",
  list: "LIST",
  set: "SET",
  zset: "ZSET",
  stream: "XS",
};

export function kindLabel(kind: string) {
  return KIND_LABELS[kind] ?? kind;
}

export function kindTag(kind: string) {
  return KIND_TAGS[kind] ?? kind.slice(0, 4).toUpperCase();
}

/** A stable id for a key, exact even when the key isn't UTF-8. */
export function keyId(key: RedisBytes) {
  return key.hex === undefined ? `t:${key.text}` : `x:${key.hex}`;
}

export function textBytes(text: string): RedisBytes {
  return { text };
}

export function sameBytes(left: RedisBytes, right: RedisBytes) {
  return keyId(left) === keyId(right);
}

/** How a value reads in a list or cell: binary values show as escaped bytes. */
export function displayBytes(value: RedisBytes, limit = 400) {
  const text = value.hex === undefined ? value.text : escapeBinary(value.hex);
  return text.length > limit ? `${text.slice(0, limit)}…` : text;
}

function escapeBinary(hex: string) {
  let out = "";
  for (let at = 0; at < hex.length; at += 2) {
    const byte = parseInt(hex.slice(at, at + 2), 16);
    if (byte >= 0x20 && byte < 0x7f && byte !== 0x5c) {
      out += String.fromCharCode(byte);
    } else {
      out += `\\x${hex.slice(at, at + 2)}`;
    }
  }
  return out;
}

export function prettyJson(text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed || !"[{".includes(trimmed[0])) {
    return null;
  }
  try {
    return JSON.stringify(JSON.parse(trimmed), null, 2);
  } catch {
    return null;
  }
}

export function formatBytesSize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

export function formatTtl(ms: number) {
  if (ms === -1) return "No expiry";
  if (ms === -2) return "Expired";
  let seconds = Math.max(0, Math.round(ms / 1000));
  const parts: string[] = [];
  for (const [size, unit] of [
    [86_400, "d"],
    [3_600, "h"],
    [60, "m"],
  ] as const) {
    if (seconds >= size) {
      parts.push(`${Math.floor(seconds / size)}${unit}`);
      seconds %= size;
    }
  }
  if (seconds || !parts.length) parts.push(`${seconds}s`);
  return parts.slice(0, 2).join(" ");
}

export type KeyTreeRow =
  | { type: "folder"; id: string; name: string; depth: number; count: number; open: boolean }
  | { type: "key"; id: string; name: string; depth: number; entry: RedisKeyEntry };

interface Folder {
  folders: Map<string, Folder>;
  keys: { name: string; entry: RedisKeyEntry }[];
  count: number;
}

function emptyFolder(): Folder {
  return { folders: new Map(), keys: [], count: 0 };
}

/**
 * Groups keys into folders by `separator`, the way `user:42:profile` reads,
 * and flattens the open folders into rows. Folders sort before keys.
 */
export function keyTree(keys: RedisKeyEntry[], separator: string, open: Set<string>): KeyTreeRow[] {
  if (!separator) {
    return [...keys]
      .sort((a, b) => a.key.text.localeCompare(b.key.text))
      .map((entry) => ({ type: "key", id: keyId(entry.key), name: entry.key.text, depth: 0, entry }));
  }
  const root = emptyFolder();
  for (const entry of keys) {
    const parts = entry.key.text.split(separator);
    const leaf = parts.pop() ?? "";
    let folder = root;
    folder.count += 1;
    for (const part of parts) {
      let next = folder.folders.get(part);
      if (!next) {
        next = emptyFolder();
        folder.folders.set(part, next);
      }
      next.count += 1;
      folder = next;
    }
    folder.keys.push({ name: leaf || entry.key.text, entry });
  }
  const rows: KeyTreeRow[] = [];
  const walk = (folder: Folder, path: string, depth: number) => {
    const names = [...folder.folders.keys()].sort((a, b) => a.localeCompare(b));
    for (const name of names) {
      const child = folder.folders.get(name)!;
      const id = `${path}${name}${separator}`;
      const isOpen = open.has(id);
      rows.push({ type: "folder", id, name, depth, count: child.count, open: isOpen });
      if (isOpen) walk(child, id, depth + 1);
    }
    const leaves = [...folder.keys].sort((a, b) => a.name.localeCompare(b.name));
    for (const leaf of leaves) {
      rows.push({ type: "key", id: keyId(leaf.entry.key), name: leaf.name, depth, entry: leaf.entry });
    }
  };
  walk(root, "", 0);
  return rows;
}

function quote(text: string) {
  return `"${text
    .replace(/\\/g, "\\\\")
    .replace(/"/g, '\\"')
    .replace(/\n/g, "\\n")
    .replace(/\r/g, "\\r")
    .replace(/\t/g, "\\t")}"`;
}

function quoteBytes(value: RedisBytes) {
  if (value.hex === undefined) return quote(value.text);
  let out = "";
  for (let at = 0; at < value.hex.length; at += 2) {
    const byte = parseInt(value.hex.slice(at, at + 2), 16);
    if (byte === 0x22 || byte === 0x5c) out += `\\${String.fromCharCode(byte)}`;
    else if (byte >= 0x20 && byte < 0x7f) out += String.fromCharCode(byte);
    else out += `\\x${value.hex.slice(at, at + 2)}`;
  }
  return `"${out}"`;
}

/** Renders a reply the way redis-cli prints it. */
export function formatReply(reply: RedisReply, indent = 0): string {
  switch (reply.type) {
    case "nil":
      return "(nil)";
    case "int":
      return `(integer) ${reply.value}`;
    case "bulk":
      return quoteBytes(reply.value);
    case "status":
      return reply.value;
    case "error":
      return `(error) ${reply.value}`;
    case "double":
      return `(double) ${reply.value ?? "nan"}`;
    case "bool":
      return reply.value ? "(true)" : "(false)";
    case "big":
      return `(big number) ${reply.value}`;
    case "array":
    case "set": {
      if (!reply.value.length) {
        return reply.type === "set" ? "(empty set)" : "(empty array)";
      }
      const width = String(reply.value.length).length;
      return reply.value
        .map((item, at) => {
          const label = `${String(at + 1).padStart(width)}) `;
          const pad = indent + label.length;
          const body = formatReply(item, pad);
          return `${at ? " ".repeat(indent) : ""}${label}${body}`;
        })
        .join("\n");
    }
    case "map": {
      if (!reply.value.length) return "(empty hash)";
      const width = String(reply.value.length).length;
      return reply.value
        .map(([key, value], at) => {
          const label = `${String(at + 1).padStart(width)}# `;
          const keyText = formatReply(key, indent + label.length);
          const prefix = `${label}${keyText} => `;
          return `${at ? " ".repeat(indent) : ""}${prefix}${formatReply(value, indent + prefix.length)}`;
        })
        .join("\n");
    }
  }
}

/** Commands that wipe data, so the console asks first. */
export function destructiveCommand(line: string) {
  const name = line.trim().split(/\s+/, 1)[0]?.toUpperCase() ?? "";
  return ["FLUSHDB", "FLUSHALL", "SHUTDOWN", "DEBUG", "SWAPDB"].includes(name) ? name : null;
}
