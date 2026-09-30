export interface ChangelogRelease {
  version: string;
  date: string;
  notes: string[];
}

export const CHANGELOG: ChangelogRelease[] = [
  {
    version: "1.1.1",
    date: "September 30, 2026",
    notes: [
      "With a table name focused in the sidebar, typing goes into Filter tables, including the first character.",
      "The card shown when a connection fails is wider, so its buttons no longer wrap their labels onto two lines.",
    ],
  },
  {
    version: "1.1.0",
    date: "September 29, 2026",
    notes: [
      "Groups and ungrouped connections now share one list on the Connections dashboard, so you can drag them into any order, like a connection between two groups.",
      "To take a connection out of a group, drop it in the gap between entries or on the top half of a group's header. Dropping it lower on a group puts it inside.",
      "Sort A–Z on the dashboard sorts groups and ungrouped connections together by name.",
      "New groups appear at the top of the dashboard.",
    ],
  },
  {
    version: "1.0.2",
    date: "September 29, 2026",
    notes: [
      "Recon is now open source under the MIT license. You can use, change, share, and sell it with no restrictions beyond keeping the copyright notice.",
      "Building Recon from source now uses Bun instead of npm. The README has the updated setup steps.",
    ],
  },
  {
    version: "1.0.1",
    date: "September 28, 2026",
    notes: [
      "Dragging a table tab along its tab strip reorders it. Dragging it onto the edge of a pane, or just past that edge, still splits off a new pane.",
    ],
  },
  {
    version: "1.0.0",
    date: "September 28, 2026",
    notes: [
      "Recon is now licensed under the Functional Source License (FSL-1.1-MIT). You can use, change, and share it freely, including at work, but not sell it as a competing product. Each release becomes MIT two years after it comes out.",
    ],
  },
  {
    version: "0.6.2",
    date: "September 28, 2026",
    notes: [
      "The active table tab now joins the line under the tab strip instead of sitting on top of it, and in split panes it matches the grid's background.",
    ],
  },
  {
    version: "0.6.1",
    date: "September 27, 2026",
    notes: [
      "The auto refresh menu beside a table's refresh button now opens above the column headers, so Off is no longer hidden behind them.",
    ],
  },
  {
    version: "0.6.0",
    date: "September 27, 2026",
    notes: [
      "Filter a table with the Filter button or ⌘F. Each condition offers the operators that fit its column type, conditions can be combined with All or Any and grouped, and values are checked against the column before the query runs. Show SQL displays the query the filters produce.",
      "Filter value fields suggest values from the table as you type, drawn from a sample of rows so large tables stay fast.",
      "Date filters can match Today, Yesterday, or Tomorrow, which stay current each time the tab loads, and time-zone-aware PostgreSQL columns use your computer's time zone.",
      "Each table tab keeps its own filters, and they're saved with your open tabs. A filtered tab shows a blue funnel, and a collapsed filter panel shows a summary of the active filters. Hover the funnel, the Filter button, or the summary to see every condition.",
      "Filters reload the rows as you edit them. Turn off Apply filters automatically in Settings to wait for Apply instead, which helps on large tables.",
      "Filtered queries and row counts can be cancelled, and a count that takes too long gives up instead of holding up the table.",
      "Double-click a table in the sidebar, or Option-click it, to open it in another tab.",
      "Split the tables view into up to six panes and drag tabs between them. Each pane has its own tabs, filters, and scroll position, and the layout is saved.",
      "Refresh a table with the refresh button or ⌘R, and choose how many rows each tab loads, up to 500. A bar under the grid shows how long the rows took to load, which rows are showing, and the page controls.",
      "Auto refresh reloads a table on a timer. Pick an interval from the arrow beside the refresh button or enter your own in minutes and seconds. A green dot on the button shows it's on, and the bar under the grid counts down to the next refresh. It waits while you have unsaved edits and stops while the table is hidden, off-screen, or Recon isn't the active window.",
      "When a tab strip overflows, its scrollbar sits above the tabs, and the mouse wheel scrolls it sideways. The wheel also scrolls a wide grid sideways when it has nothing to scroll vertically.",
      "Choose Duplicate connection from a connection's menu to open a new connection form filled in with its settings, including SSH, and named Copy of followed by the original name. Passwords and SSH secrets aren't copied, so enter them before saving.",
    ],
  },
  {
    version: "0.5.0",
    date: "September 26, 2026",
    notes: [
      "Save a query tab's SQL to a .sql file with the Export .sql button at the right end of the Run bar.",
      "Right-click a saved query and choose Export .sql to save its SQL to a file.",
      "Beautify, next to Export .sql in the Run bar, lays out a query tab's SQL with line breaks and indentation so long statements are easier to read. Select part of the SQL first to format just that part, or press Shift+Option+F.",
    ],
  },
  {
    version: "0.4.0",
    date: "September 26, 2026",
    notes: [
      "Back up a whole database or schema with the Backup button next to Export. A backup always includes the structure, data, routines, and triggers, is gzipped, and lists anything Recon couldn't include.",
      "Restore replaces everything in the current database or schema with a backup, so tables added since the backup are removed rather than left behind. On PostgreSQL the restore runs in one transaction, so a failure or cancel changes nothing. It refuses to run when something outside depends on what's being restored, like a view or foreign key in another schema or database, and names what's in the way. On MySQL, events are kept.",
      "PostgreSQL exports now include the enums, domains, composite types, and range types the tables use, along with the extensions they need, so the file imports into a fresh database. Types that already exist where you import are kept as they are.",
      "Exports now include triggers on MySQL and PostgreSQL. They're added after the data, so importing doesn't run them on rows that already went through them.",
      "Exporting a whole database now includes its stored functions and procedures. Exporting selected PostgreSQL tables includes just the functions those tables use.",
      "PostgreSQL exports keep table, view, and column comments.",
      "PostgreSQL exports include sequences that column defaults use even when no exported table owns them, and keep each sequence's settings and current position.",
      "Views that read from other views are exported in an order that imports cleanly.",
      "Importing a PostgreSQL export over an earlier import of it no longer fails when one table's default uses another table's sequence.",
      "Drag a connection by its handle to move it into another group, onto a collapsed group, or out of its group. If none of your connections are outside a group, a drop area appears below the groups while you drag.",
    ],
  },
  {
    version: "0.3.0",
    date: "September 25, 2026",
    notes: [
      "Foreign key columns show an arrow next to each value. Click it to open the referenced table showing just that record, and clear the filter from the toolbar to see every row again.",
      "Hovering a foreign key column's header shows the table and column it points to.",
      "History moved from the top bar to a History tab next to Tables and SQL, and now shows only that connection's queries. Pause and Clear apply to the current connection, and deleting a connection removes its history.",
      "Export a whole database to a .sql file with the Export button in the connection toolbar, or right-click a table and choose Export table. Choose structure, data, or both, whether to drop existing tables first, and gzip compression, which is on by default.",
      "Cmd+click tables in the sidebar to select several, or Shift+click to select a range, then right-click any of them to export just those tables.",
      "Import runs a .sql or .sql.gz file against the current database and shows its progress. It reads dumps from mysqldump and pg_dump's plain format, including COPY data, as well as Recon's own exports.",
      "New records skip auto-increment columns, which show auto until you save. The database then assigns the next id, and it appears in the grid. Double-click the cell to set an id yourself, or press Backspace to go back to auto.",
      "On PostgreSQL, ids you set yourself also work for GENERATED ALWAYS identity columns, and the column's sequence moves past them so later automatic ids don't collide.",
      "Double-click a query tab, or right-click it and choose Rename, to rename it.",
      "Right-click a query tab and choose Save query, or press Cmd+S in the SQL tab, to save it with a name and an optional description. Saved queries belong to their connection and are stored in settings.json.",
      "Saved queries appear in a Saved queries tab pinned at the start of the SQL tabs. Pick one to see its SQL, edit its name or description, or open or run it in a tab. Double-click one to open it, or right-click it to delete it.",
      "A tab opened from a saved query shows a bookmark icon, and it's marked as changed when its SQL no longer matches the saved version. Cmd+S saves the changes, and renaming the tab renames the saved query.",
      "Right-click a database in the database switcher and choose Hide from list to hide it. Click Show hidden databases at the bottom of the list to see them again, then right-click one and choose Show in list to bring it back.",
    ],
  },
  {
    version: "0.2.1",
    date: "September 25, 2026",
    notes: [
      "Double-click the bar between the table list and the table view to fit the list to the longest table name.",
      "Fixed an error that sometimes appeared after dragging that bar to resize the table list.",
    ],
  },
  {
    version: "0.2.0",
    date: "September 25, 2026",
    notes: [
      "Edit table data: double-click a cell (or press Enter or start typing) to change it, and press Backspace to set it to NULL.",
      "Edits stay unsaved until you press Cmd+S or Save, which writes every edited table in one transaction. Discard reverts them all.",
      "Unsaved cells, rows, tables in the sidebar, and table tabs are highlighted, and closing a tab with unsaved edits asks first.",
      "Cmd+Z and Cmd+Shift+Z undo and redo edits in the current table.",
      "Views and tables without a primary key stay read-only.",
      "Clicking a cell now highlights its whole row.",
      "Cmd+R reloads the current table.",
      "Saved edits appear in History as Edit queries.",
      "Indexes now have their own tab next to Data and Structure, and both use the same grid as table data. The Data, Structure, and Indexes buttons show the row, column, and index counts.",
      "Edit a table's structure the same way as its data: double-click a column's name, type, nullable, or default, then save with Cmd+S alongside any row edits. Undo, Discard, and History work the same.",
      "Defaults in Structure are shown and edited as SQL, like 'draft' or CURRENT_TIMESTAMP. Press Backspace on a default to remove it.",
      "Changing a MySQL column keeps its collation, comment, auto_increment, and ON UPDATE. SQLite columns can only be renamed.",
      "Add records, columns, and indexes with the New record, New column, or New index button, or by double-clicking empty space in the grid. New rows are marked with + and saved with Cmd+S like any other edit.",
      "Tab and Shift+Tab move to the next or previous field and keep editing, so you can fill in a whole row from the keyboard.",
      "Indexes are editable too: rename them, change their columns, or switch between UNIQUE and INDEX. PostgreSQL and SQLite recreate the index, keeping its method and WHERE clause. Primary keys stay read-only.",
      "Connect to remote MySQL and PostgreSQL servers through an SSH tunnel, signing in with a password, a private key, or your SSH agent.",
      "Private keys in ~/.ssh are listed so you can pick one, and SSH passwords and key passphrases are stored in the macOS Keychain.",
      "New SSH hosts are added to ~/.ssh/known_hosts on first connect, and Recon refuses to connect if a host key changes.",
      "Dragging to select text in a dialog no longer closes it when the mouse is released outside.",
      "The editor font now defaults to 14px and the grid font to 13px.",
      "Settings has a new Table list font and size for the table names in the connection sidebar.",
      "Table data and query results now fill the whole pane with a continuous grid, even when there are only a few rows or columns.",
      "Recon reconnects on its own when a connection or SSH tunnel drops, for example after sleep or a server restart, and stays on the database you were using.",
      "If reconnecting fails, a Connection lost banner explains why and offers Reconnect, keeping your tabs and unsaved edits. The Reconnect button in the table sidebar is gone.",
      "The table list refreshes on its own when you switch to a connection tab or come back to Recon, so tables from migrations or other clients show up without the old Reload tables button.",
      "A query that was interrupted by a dropped connection is not re-run automatically, since it may already have run.",
      "The database switcher has a New database option at the top that creates a database and switches to it. On PostgreSQL it creates a schema.",
      "Right-click a database in the switcher and choose Drop to delete it after confirming. The database you're currently using can't be dropped.",
      "The same right-click menu can open a database in a new tab, copy its name, or rename it. MySQL renames by moving the tables into a new database, and can't rename one that has views, triggers, routines, or events.",
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
