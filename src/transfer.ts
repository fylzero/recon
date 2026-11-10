import { save } from "@tauri-apps/plugin-dialog";
import * as api from "./api";
import type { Driver, ImportProgress, ImportResume, ImportResumePoint, TabularOptions } from "./types";

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

/** Formats a transfer duration in milliseconds for completion toasts, matching the dashboard clock when >= 1 minute. */
export function formatDuration(durationMs: number) {
  if (!Number.isFinite(durationMs) || durationMs <= 0) {
    return "0.0s";
  }
  if (durationMs < 100) {
    return `${Math.max(1, Math.round(durationMs))}ms`;
  }
  const totalSeconds = durationMs / 1000;
  if (totalSeconds < 60) {
    return `${totalSeconds.toFixed(1)}s`;
  }
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  const secText = seconds.toFixed(1).padStart(4, "0");
  if (hours > 0) {
    return `${hours}h ${minutes}m ${secText}s`;
  }
  return `${minutes}m ${secText}s`;
}

/** Picks a stopped import or restore up again, from `point.from` to run the uncertain statements again or `point.to` to skip them. */
export function resumeAt(point: ImportResumePoint, skip: number): ImportResume {
  return {
    skip,
    line: point.line,
    fingerprint: point.fingerprint,
    backend: point.backend,
    parallel: point.parallel,
    splitTables: point.splitTables,
    done: point.done,
    skipped: skip === point.from ? [] : point.uncertain,
  };
}

/** Explains what resuming from `point` does, and the choice to make when some statements may or may not have run. */
export function resumeChoice(point: ImportResumePoint) {
  const line = point.line.toLocaleString();
  if (point.to === point.from) {
    return `Resume picks up at line ${line}.`;
  }
  const single = point.to - point.from === 1;
  const what = single ? `The statement on line ${line}` : `The statements from line ${line} on`;
  const it = single ? "it" : "them";
  if (point.likelySaved) {
    return `${what} most likely ran before the connection dropped, so skip ${it} and carry on.`;
  }
  return `${what} may or may not have run before the connection dropped. Skip ${it} if the rows are already there, or run ${it} again if they aren't.`;
}

/** The status line while a file runs, or null while it's simply running. */
export function runPhase(progress: ImportProgress | null) {
  if (progress?.phase === "reconnecting") {
    return progress.attempt ? `Connection lost. Reconnecting (attempt ${progress.attempt + 1})…` : "Connection lost. Reconnecting…";
  }
  if (progress?.phase === "skipping") {
    return "Skipping what already ran…";
  }
  return null;
}

/** One step of an import or restore, as the dashboard shows it. */
export interface ImportStage {
  id: "schema" | "rows" | "statements";
  title: string;
  description: string;
  percent: number;
  /** Its share of the whole run, relative to the other stages. */
  weight: number;
}

function share(done: number, total: number) {
  return total > 0 ? Math.min(100, Math.max(0, (done / total) * 100)) : 0;
}

/** The steps of a run, from its latest progress. */
export function importStages(progress: ImportProgress | null): ImportStage[] {
  const stages: ImportStage[] = [];
  if (!progress) {
    stages.push({ id: "statements", title: "Import", description: "Starting", percent: 0, weight: 7 });
    return stages;
  }
  if (!progress.workers.length) {
    stages.push({
      id: "statements",
      title: "Run statements",
      description: "One at a time, in order",
      percent: share(progress.bytes, progress.totalBytes),
      weight: 7,
    });
    return stages;
  }
  const planning = progress.phase === "planning";
  let run = 0;
  let total = 0;
  for (const worker of progress.workers) {
    run += worker.statements;
    total += worker.totalStatements;
  }
  if (!total) {
    run = progress.statements;
    total = progress.totalStatements;
  }
  stages.push({
    id: "schema",
    title: "Create tables",
    description: "Split the file and create tables",
    percent: planning ? share(progress.fileBytes, progress.fileTotalBytes) : 100,
    weight: 2,
  });
  stages.push({
    id: "rows",
    title: "Insert rows",
    description: `Over ${progress.workers.length} connections`,
    percent: planning ? 0 : share(run, total),
    weight: 5,
  });
  return stages;
}

/** The whole run's percent across `stages`, by weight. */
export function stagesPercent(stages: ImportStage[]) {
  const weight = stages.reduce((sum, stage) => sum + stage.weight, 0);
  if (!weight) {
    return 0;
  }
  return stages.reduce((sum, stage) => sum + stage.percent * stage.weight, 0) / weight;
}

/** What a backup never includes, whatever the dumper manages to write. */
export function backupLimits(driver: Driver) {
  if (driver === "mysql") {
    return "Events aren't backed up. Restoring leaves the existing events in place.";
  }
  if (driver === "postgres") {
    return "Grants, owners of individual objects, row-level security policies, custom collations and operators, and extended statistics aren't backed up.";
  }
  if (driver === "mssql") {
    return "Logins, users, permissions, and assemblies aren't backed up.";
  }
  return "";
}
