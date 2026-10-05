import { save } from "@tauri-apps/plugin-dialog";
import * as api from "./api";
import type { Driver, TabularOptions } from "./types";

export function fileSafe(name: string) {
  return name.replace(/[/\\:*?"<>|]+/g, "-");
}

const TABULAR_KEY = "recon.tabularOptions";

export const DEFAULT_TABULAR: TabularOptions = {
  format: "csv",
  delimiter: ",",
  header: true,
  nullAs: "",
  bom: false,
  jsonStyle: "array",
  gzip: false,
};

/** The CSV and JSON choices from the last export, so every export dialog starts from them. */
export function loadTabularOptions(): TabularOptions {
  try {
    const saved = JSON.parse(localStorage.getItem(TABULAR_KEY) ?? "{}");
    const options = { ...DEFAULT_TABULAR };
    if (saved?.format === "csv" || saved?.format === "json") {
      options.format = saved.format;
    }
    if ([",", "\t", ";", "|"].includes(saved?.delimiter)) {
      options.delimiter = saved.delimiter;
    }
    if (["", "\\N", "NULL"].includes(saved?.nullAs)) {
      options.nullAs = saved.nullAs;
    }
    if (saved?.jsonStyle === "array" || saved?.jsonStyle === "lines") {
      options.jsonStyle = saved.jsonStyle;
    }
    for (const key of ["header", "bom", "gzip"] as const) {
      if (typeof saved?.[key] === "boolean") {
        options[key] = saved[key];
      }
    }
    return options;
  } catch {
    return { ...DEFAULT_TABULAR };
  }
}

export function saveTabularOptions(options: TabularOptions) {
  localStorage.setItem(TABULAR_KEY, JSON.stringify(options));
}

/** Matches the extension the backend gives each file. */
export function tabularExtension(options: TabularOptions) {
  const base =
    options.format === "json" ? (options.jsonStyle === "lines" ? "ndjson" : "json") : options.delimiter === "\t" ? "tsv" : "csv";
  return options.gzip ? `${base}.gz` : base;
}

export function tabularFilter(options: TabularOptions) {
  const extension = tabularExtension(options);
  const name = options.format === "json" ? "JSON" : options.delimiter === "\t" ? "TSV" : "CSV";
  return options.gzip ? { name: `Gzipped ${name}`, extensions: ["gz"] } : { name, extensions: [extension] };
}

/** Asks where to save a CSV or JSON file named after `name`. Resolves to null if cancelled. */
export async function chooseTabularPath(title: string, name: string, options: TabularOptions) {
  return save({
    title,
    defaultPath: `${fileSafe(name) || "export"}.${tabularExtension(options)}`,
    filters: [tabularFilter(options)],
  });
}

const IMPORT_EXTENSIONS = /\.(csv|tsv|txt|json|ndjson|jsonl)(\.gz)?$/i;

/** Whether a picked file holds rows as CSV or JSON, rather than SQL statements. */
export function isTabularFile(path: string) {
  return IMPORT_EXTENSIONS.test(path);
}

/** Asks where to save the SQL and writes it. Resolves to the path, or null if cancelled. */
export async function exportSqlFile(name: string, sql: string) {
  const base = fileSafe(name.trim()).replace(/\.sql$/i, "") || "query";
  const path = await save({
    title: "Export SQL",
    defaultPath: `${base}.sql`,
    filters: [{ name: "SQL", extensions: ["sql"] }],
  });
  if (!path) {
    return null;
  }
  await api.writeTextFile(path, sql.endsWith("\n") || !sql ? sql : `${sql}\n`);
  return path;
}

export function formatBytes(bytes: number) {
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${unit ? value.toFixed(1) : value} ${units[unit]}`;
}

/** What a backup never includes, whatever the dumper manages to write. */
export function backupLimits(driver: Driver) {
  if (driver === "mysql") {
    return "Events aren't backed up. Restoring leaves the existing events in place.";
  }
  if (driver === "postgres") {
    return "Grants, owners of individual objects, row-level security policies, custom collations and operators, and extended statistics aren't backed up.";
  }
  return "";
}
