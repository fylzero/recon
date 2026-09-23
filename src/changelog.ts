export interface ChangelogRelease {
  version: string;
  date: string;
  notes: string[];
}

export const CHANGELOG: ChangelogRelease[] = [
  {
    version: "0.1.0",
    date: "September 23, 2026",
    notes: [
      "First release: a local-first macOS database client for MySQL, PostgreSQL, and SQLite.",
      "Connections dashboard with colored groups, drag to reorder, and Sort A–Z.",
      "Passwords are stored in the macOS Keychain, never in settings.json.",
      "Connection tabs with a table sidebar, paged Data view, Structure view, and SQL query tabs.",
      "Query editor with SQL highlighting, table and column autocomplete, Cmd+Enter to run, and Cancel.",
      "History tab records every query Recon runs, with pause and clear.",
    ],
  },
];
