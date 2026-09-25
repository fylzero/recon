export interface ChangelogRelease {
  version: string;
  date: string;
  notes: string[];
}

export const CHANGELOG: ChangelogRelease[] = [
  {
    version: "0.2.0",
    date: "September 25, 2026",
    notes: [
      "Connect to remote MySQL and PostgreSQL servers through an SSH tunnel, signing in with a password, a private key, or your SSH agent.",
      "Private keys in ~/.ssh are listed so you can pick one, and SSH passwords and key passphrases are stored in the macOS Keychain.",
      "New SSH hosts are added to ~/.ssh/known_hosts on first connect, and Recon refuses to connect if a host key changes.",
      "Dragging to select text in a dialog no longer closes it when the mouse is released outside.",
    ],
  },
  {
    version: "0.1.6",
    date: "September 25, 2026",
    notes: [
      "Connections now have Tables and SQL tabs. The SQL tab hides the table sidebar so queries get the full width, and a + button opens another query.",
      "Larger driver icons in the connections list.",
    ],
  },
  {
    version: "0.1.5",
    date: "September 24, 2026",
    notes: [
      "Recon is now signed with a Developer ID and notarized by Apple, so macOS opens it without an unidentified developer warning.",
      "Connection toolbar with a database switcher.",
      "Result columns size themselves to fit their data, up to a Max column width you can set in Settings.",
    ],
  },
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
