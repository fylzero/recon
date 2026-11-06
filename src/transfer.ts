import { save } from "@tauri-apps/plugin-dialog";
import * as api from "./api";
import type { Driver } from "./types";

export function fileSafe(name: string) {
  return name.replace(/[/\\:*?"<>|]+/g, "-");
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
  if (driver === "mssql") {
    return "Logins, users, permissions, and assemblies aren't backed up.";
  }
  return "";
}
