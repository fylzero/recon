import { save } from "@tauri-apps/plugin-dialog";
import * as api from "./api";
import type { Driver, ImportProgress, ImportResume, ImportResumePoint, ImportWorkerProgress, TabularOptions } from "./types";

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
  return { skip, line: point.line, fingerprint: point.fingerprint, backend: point.backend };
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

/** SQL load stage after stripping `reconnecting` / `skipping` overlays. */
export type SqlLoadPhase = "planning" | "running" | "finishing";

export interface LoadPhasePercents {
  phase1: number;
  phase2: number;
  phase3: number;
  schema: number;
  file: number;
  index: number;
  fk: number;
  view: number;
  trigger: number;
}

function percentOf(done: number, total: number, empty = 0) {
  if (!total) {
    return empty;
  }
  return Math.min(100, Math.max(0, (done / total) * 100));
}

function definedNumber(value: number | undefined): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function hasSchemaCounts(progress: ImportProgress | null): boolean {
  return Boolean(progress && (definedNumber(progress.schemaStatements) || definedNumber(progress.schemaTotalStatements)));
}

function hasFinalizeBuckets(progress: ImportProgress | null): boolean {
  return Boolean(
    progress &&
      (definedNumber(progress.indexStatements) ||
        definedNumber(progress.indexTotalStatements) ||
        definedNumber(progress.fkStatements) ||
        definedNumber(progress.fkTotalStatements) ||
        definedNumber(progress.viewStatements) ||
        definedNumber(progress.viewTotalStatements) ||
        definedNumber(progress.triggerStatements) ||
        definedNumber(progress.triggerTotalStatements)),
  );
}

/** Explicit 0/0 is complete; omitted fields stay absent so a thin overlay cannot inflate Finalize. */
function bucketPercent(done: number | undefined, total: number | undefined): number | null {
  if (!definedNumber(done) && !definedNumber(total)) {
    return null;
  }
  return percentOf(done ?? 0, total ?? 0, 100);
}

function workersCaughtUp(workers: ImportWorkerProgress[]) {
  return workers.length > 0 && workers.every((worker) => worker.state === "idle" || worker.state === "done");
}

/** Phase 1 worker rows only — never job-level insert totals. */
function schemaWorkerPercent(progress: ImportProgress | null): number {
  if (!progress?.workers.length) {
    return 0;
  }
  let run = 0;
  let total = 0;
  for (const worker of progress.workers) {
    run += worker.statements;
    total += worker.totalStatements;
  }
  if (!total) {
    return workersCaughtUp(progress.workers) ? 100 : 0;
  }
  return percentOf(run, total, 0);
}

/** Worker statement totals, falling back to the job-level counts when workers have none. */
export function statementCounts(progress: ImportProgress | null): { run: number; total: number } {
  if (!progress) {
    return { run: 0, total: 0 };
  }
  let run = 0;
  let total = 0;
  for (const worker of progress.workers) {
    run += worker.statements;
    total += worker.totalStatements;
  }
  if (total === 0 && progress.totalStatements > 0) {
    run = progress.statements;
    total = progress.totalStatements;
  }
  run = Math.min(total, Math.max(0, run));
  return { run, total };
}

/** Insert counts only. Ignores Phase 1 schema workers so leftover insert totals cannot drive Setup. */
export function insertCounts(progress: ImportProgress | null): { run: number; total: number } {
  if (!progress) {
    return { run: 0, total: 0 };
  }
  if (progress.phase === "running" || progress.phase === "skipping") {
    return statementCounts(progress);
  }
  const total = Math.max(0, progress.totalStatements);
  const run = Math.min(total, Math.max(0, progress.statements));
  return { run, total };
}

/**
 * Maps a progress event onto the three SQL stages. `skipping` counts as inserts;
 * `reconnecting` keeps the previous stage. A first overlay with no stage yet
 * stays on planning while the dump is still being read, otherwise inserts.
 */
export function sqlLoadPhase(progress: ImportProgress | null, previous: SqlLoadPhase = "planning"): SqlLoadPhase {
  if (!progress || progress.phase === "planning") {
    return "planning";
  }
  if (progress.phase === "finishing") {
    return "finishing";
  }
  if (progress.phase === "running" || progress.phase === "skipping") {
    return "running";
  }
  if (previous !== "planning") {
    return previous;
  }
  const stillReading = progress.fileTotalBytes > 0 && progress.fileBytes < progress.fileTotalBytes;
  if (stillReading && progress.totalStatements === 0) {
    return "planning";
  }
  return "running";
}

function setupPercent(progress: ImportProgress | null, phase: SqlLoadPhase): number {
  if (!progress) {
    return 0;
  }
  if (phase !== "planning") {
    return 100;
  }
  const filePct = percentOf(progress.fileBytes, progress.fileTotalBytes, 100);
  if (!hasSchemaCounts(progress)) {
    return percentOf(progress.fileBytes, progress.fileTotalBytes, 0);
  }
  const schemaPct = percentOf(progress.schemaStatements ?? 0, progress.schemaTotalStatements ?? 0, 100);
  return (schemaPct + filePct) / 2;
}

function insertPercent(progress: ImportProgress | null, phase: SqlLoadPhase): number {
  if (!progress || phase === "planning") {
    return 0;
  }
  if (phase === "finishing") {
    return 100;
  }
  const counts = insertCounts(progress);
  return percentOf(counts.run, counts.total, 0);
}

function finalizePercent(progress: ImportProgress | null, phase: SqlLoadPhase): LoadPhasePercents {
  const empty: LoadPhasePercents = {
    phase1: 0,
    phase2: 0,
    phase3: 0,
    schema: 0,
    file: 0,
    index: 0,
    fk: 0,
    view: 0,
    trigger: 0,
  };
  if (!progress || phase !== "finishing") {
    return { ...empty };
  }
  if (hasFinalizeBuckets(progress)) {
    const index = bucketPercent(progress.indexStatements, progress.indexTotalStatements);
    const fk = bucketPercent(progress.fkStatements, progress.fkTotalStatements);
    const view = bucketPercent(progress.viewStatements, progress.viewTotalStatements);
    const trigger = bucketPercent(progress.triggerStatements, progress.triggerTotalStatements);
    const present = [index, fk, view, trigger].filter((value): value is number => value !== null);
    const phase3 = present.length ? present.reduce((sum, value) => sum + value, 0) / present.length : 0;
    return {
      ...empty,
      phase3,
      index: index ?? 0,
      fk: fk ?? 0,
      view: view ?? 0,
      trigger: trigger ?? 0,
    };
  }
  const counts = statementCounts(progress);
  const aggregate = percentOf(counts.run, counts.total, 100);
  return { ...empty, phase3: aggregate, index: aggregate, fk: aggregate, view: aggregate, trigger: aggregate };
}

/** Percents for the three phase donuts and their sub-donuts. 0/0 buckets count as complete. */
export function loadRingPercents(progress: ImportProgress | null, phase: SqlLoadPhase): LoadPhasePercents {
  if (!progress) {
    return { phase1: 0, phase2: 0, phase3: 0, schema: 0, file: 0, index: 0, fk: 0, view: 0, trigger: 0 };
  }
  const finishing = finalizePercent(progress, phase);
  const schema = hasSchemaCounts(progress)
    ? percentOf(progress?.schemaStatements ?? 0, progress?.schemaTotalStatements ?? 0, 100)
    : schemaWorkerPercent(progress);
  const file = percentOf(progress?.fileBytes ?? 0, progress?.fileTotalBytes ?? 0, 100);
  return {
    phase1: setupPercent(progress, phase),
    phase2: insertPercent(progress, phase),
    phase3: finishing.phase3,
    schema: phase === "planning" ? schema : 100,
    file: phase === "planning" ? file : 100,
    index: finishing.index,
    fk: finishing.fk,
    view: finishing.view,
    trigger: finishing.trigger,
  };
}

/** Mean of the three phase donuts, never 100 until every phase is full so the job bar can't freeze during Finalize. */
export function overallLoadPercent(rings: { phase1: number; phase2: number; phase3: number }): number {
  const mean = (rings.phase1 + rings.phase2 + rings.phase3) / 3;
  if (rings.phase1 >= 100 && rings.phase2 >= 100 && rings.phase3 >= 100) {
    return 100;
  }
  return Math.min(99, Math.round(mean));
}

export function loadJobPercent(progress: ImportProgress | null, phase: SqlLoadPhase): number {
  return overallLoadPercent(loadRingPercents(progress, phase));
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
