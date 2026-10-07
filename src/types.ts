import type { WireNode } from "./filters/compile";
import type { FilterKind } from "./filters/model";

export type Driver = "mysql" | "postgres" | "sqlite";
export type SslMode = "disable" | "prefer" | "require";

export const DRIVER_OPTIONS: { id: Driver; label: string; defaultPort: number }[] = [
  { id: "mysql", label: "MySQL", defaultPort: 3306 },
  { id: "postgres", label: "PostgreSQL", defaultPort: 5432 },
  { id: "sqlite", label: "SQLite", defaultPort: 0 },
];

export function driverLabel(driver: Driver) {
  return DRIVER_OPTIONS.find((option) => option.id === driver)?.label ?? driver;
}

export type SshAuth = "password" | "key" | "agent";

export interface SshTunnel {
  enabled: boolean;
  host: string;
  port: number;
  user: string;
  auth: SshAuth;
  keyPath: string;
  alsoPassword?: boolean;
}

export interface ResolvedSshHost {
  hostname: string;
  port: number | null;
  user: string;
  identityFiles: string[];
  identityAgent: string;
  proxyJump: string;
  proxyCommand: string;
}

export interface AuthPrompt {
  id: string;
  title: string;
  instructions: string;
  prompt: string;
  secret: boolean;
}

export const DEFAULT_SSH_PORT = 22;

export function defaultSshTunnel(): SshTunnel {
  return {
    enabled: false,
    host: "",
    port: DEFAULT_SSH_PORT,
    user: "",
    auth: "password",
    keyPath: "",
    alsoPassword: false,
  };
}

export interface ConnectionEntry {
  id: string;
  name: string;
  driver: Driver;
  host: string;
  port: number;
  user: string;
  database: string;
  filePath: string;
  sslMode: SslMode;
  headerColor: string;
  savePassword: boolean;
  cleartextAuth?: boolean;
  ssh?: SshTunnel;
}

export interface ConnectionGroup {
  id: string;
  name: string;
  expanded: boolean;
  headerColor?: string;
  connections: ConnectionEntry[];
}

export interface WindowState {
  x: number;
  y: number;
  width: number;
  height: number;
  maximized?: boolean;
}

export const MIN_WINDOW_WIDTH = 960;
export const MIN_WINDOW_HEIGHT = 640;
export const DEFAULT_WINDOW_WIDTH = 1280;
export const DEFAULT_WINDOW_HEIGHT = 800;

export interface SavedQuery {
  id: string;
  connectionId: string;
  name: string;
  description: string;
  sql: string;
  updatedAt: number;
}

export interface AppData {
  groups: ConnectionGroup[];
  connections?: ConnectionEntry[];
  dashboardOrder?: string[];
  savedQueries?: SavedQuery[];
  editorFontFamily?: string;
  editorFontSize?: number;
  gridFontFamily?: string;
  gridFontSize?: number;
  listFontFamily?: string;
  listFontSize?: number;
  pageSize?: number;
  queryRowLimit?: number;
  sidebarWidth?: number;
  maxAutoColumnWidth?: number;
  autoApplyFilters?: boolean;
  notifications?: NotificationMode;
  window?: WindowState;
}

export type NotificationMode = "background" | "always" | "off";

export interface PreferencesPatch {
  editorFontFamily?: string;
  editorFontSize?: number;
  gridFontFamily?: string;
  gridFontSize?: number;
  listFontFamily?: string;
  listFontSize?: number;
  pageSize?: number;
  queryRowLimit?: number;
  sidebarWidth?: number;
  maxAutoColumnWidth?: number;
  autoApplyFilters?: boolean;
  notifications?: NotificationMode;
}

export interface BytesCell {
  bytes: number;
  hex: string;
}

export type Cell = null | boolean | number | string | BytesCell;
export type RowValues = Cell[];

export interface ColumnMeta {
  name: string;
  typeName: string;
}

export interface NamespaceList {
  items: string[];
  current: string;
}

export interface SessionInfo {
  serverVersion: string;
  namespaces: NamespaceList;
  namespaceLabel: string;
  systemNamespaces: string[];
}

export interface TableInfo {
  name: string;
  kind: "table" | "view";
}

export interface DropOptions {
  disableForeignKeys: boolean;
  cascade: boolean;
}

export interface TruncateOptions extends DropOptions {
  restartIdentity: boolean;
}

export interface ColumnDetail {
  name: string;
  dataType: string;
  nullable: boolean;
  defaultValue: string | null;
  primaryKey: boolean;
  extra: string;
  filterKind: FilterKind;
  enumValues: string[];
}

export interface IndexInfo {
  name: string;
  columns: string;
  unique: boolean;
  primary: boolean;
}

/* `columns[i]` references `refColumns[i]`; composite keys have several. */
export interface ForeignKey {
  name: string;
  columns: string[];
  refNamespace: string;
  refTable: string;
  refColumns: string[];
}

export interface TableStructure {
  columns: ColumnDetail[];
  indexes: IndexInfo[];
  foreignKeys: ForeignKey[];
}

export interface SchemaColumn {
  table: string;
  column: string;
}

export interface DiagramColumn {
  name: string;
  dataType: string;
  primaryKey: boolean;
}

export interface DiagramTable {
  name: string;
  columns: DiagramColumn[];
}

export interface DiagramForeignKey extends ForeignKey {
  table: string;
}

export interface SchemaDiagram {
  tables: DiagramTable[];
  foreignKeys: DiagramForeignKey[];
}

export type SortDirection = "asc" | "desc";

export interface BrowseRequest {
  namespace: string;
  table: string;
  offset: number;
  limit: number;
  orderBy?: string | null;
  orderDir?: SortDirection | null;
  count: boolean;
  filter?: WireNode | null;
  /** Minutes east of UTC. */
  utcOffset?: number;
  /** Lets `cancelBrowse` stop the query while it runs. */
  requestId?: string;
}

/* Opens `table` showing only rows where every filter column equals its value. */
export interface TableLink {
  namespace: string;
  table: string;
  filter: CellEdit[];
}

export interface BrowseResult {
  columns: ColumnMeta[];
  rows: RowValues[];
  offset: number;
  total: number | null;
  durationMs: number;
}

/** `now` has the database fill in its current date, time, or timestamp when the row is saved. */
export type EditValue = null | boolean | number | string | { now: "date" | "time" | "datetime" };

export interface CellEdit {
  column: string;
  value: EditValue;
}

export interface RowUpdate {
  key: CellEdit[];
  changes: CellEdit[];
}

/* Only changed fields are set. A `defaultValue` of null drops the default. */
export interface ColumnChange {
  column: string;
  name?: string;
  dataType?: string;
  nullable?: boolean;
  defaultValue?: string | null;
}

/* Only changed fields are set. `columns` is the SQL column list. */
export interface IndexChange {
  index: string;
  name?: string;
  columns?: string;
  unique?: boolean;
}

export interface RowInsert {
  values: CellEdit[];
}

export interface RowDelete {
  key: CellEdit[];
}

export interface NewColumn {
  name: string;
  dataType: string;
  nullable: boolean;
  defaultValue: string | null;
}

export interface NewIndex {
  name: string;
  columns: string;
  unique: boolean;
}

export interface SaveRequest {
  namespace: string;
  table: string;
  updates: RowUpdate[];
  inserts: RowInsert[];
  deletes: RowDelete[];
  columns: ColumnChange[];
  newColumns: NewColumn[];
  indexes: IndexChange[];
  newIndexes: NewIndex[];
}

export interface StatementResult {
  sql: string;
  resultId: string | null;
  columns: ColumnMeta[];
  rows: RowValues[];
  rowCount: number;
  truncated: boolean;
  rowsAffected: number | null;
  durationMs: number;
  error: string | null;
}

export interface ExportRequest {
  namespace: string;
  /** Every table and view in the namespace when null. */
  tables: string[] | null;
  path: string;
  gzip: boolean;
  structure: boolean;
  data: boolean;
  dropTables: boolean;
  /** Rows as CSV or JSON instead of SQL. `path` is a folder when there's more than one table. */
  tabular?: TabularOptions | null;
}

export type TabularFormat = "csv" | "json";
export type JsonStyle = "array" | "lines";

export interface TabularOptions {
  format: TabularFormat;
  delimiter: string;
  header: boolean;
  /** How NULL is written in CSV: "", "\\N", or "NULL". */
  nullAs: string;
  /** A UTF-8 byte order mark, which Excel needs to read the file as UTF-8. */
  bom: boolean;
  jsonStyle: JsonStyle;
  gzip: boolean;
}

export interface ImportFileOptions {
  format?: TabularFormat | null;
  /** Detected from the first line when null. */
  delimiter?: string | null;
  header: boolean;
  /** Unquoted CSV fields that mean NULL. */
  nullMarkers: string[];
}

export interface ImportPreview {
  fileName: string;
  format: TabularFormat;
  delimiter: string;
  columns: string[];
  /** A guessed column type for each column, for creating a table. */
  types: string[];
  rows: (string | null)[][];
  sampled: number;
  totalBytes: number;
}

export interface ColumnMapping {
  /** Index into `sourceColumns`. */
  source: number;
  target: string;
}

export interface NewTable {
  columns: { name: string; dataType: string }[];
  idColumn: boolean;
}

export interface ImportRowsRequest {
  namespace: string;
  table: string;
  path: string;
  file: ImportFileOptions;
  sourceColumns: string[];
  mapping: ColumnMapping[];
  create?: NewTable | null;
  emptyFirst: boolean;
  skipConflicts: boolean;
}

export interface ImportRowsResult {
  rows: number;
  durationMs: number;
  created: boolean;
}

export interface ImportRowsProgress {
  transferId: string;
  rows: number;
  bytes: number;
  totalBytes: number;
}

export interface ExportResult {
  tables: number;
  rows: number;
  skipped: string[];
  bytes: number;
  path: string;
}

export interface ExportProgress {
  transferId: string;
  table: string;
  index: number;
  total: number;
  rows: number;
}

export interface ImportResult {
  statements: number;
  durationMs: number;
}

export interface ImportProgress {
  transferId: string;
  statements: number;
  bytes: number;
  totalBytes: number;
}

export interface BackupInfo {
  version: number;
  driver: Driver;
  namespace: string;
  server: string;
  /** RFC 3339, in UTC. */
  createdAt: string;
  /** Tables and routines the backup left out, each with the reason. */
  skipped: string[];
}

export type QueryOrigin = "editor" | "browse" | "schema" | "edit";

export interface QueryLogEntry {
  id: string;
  at: number;
  connectionId: string;
  connection: string;
  driver: string;
  database: string;
  sql: string;
  origin: QueryOrigin;
  success: boolean;
  durationMs: number;
  rows?: number;
  error: string;
}
