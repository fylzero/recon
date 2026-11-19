import type { WireNode } from "./filters/compile";
import type { FilterKind } from "./filters/model";

export type Driver = "mysql" | "postgres" | "sqlite" | "mssql" | "redis" | "mongodb";
export type SslMode = "disable" | "prefer" | "require" | "verify-ca" | "verify-full";
export type PasswordSource = "password" | "none" | "aws-iam" | "command";

/** Servers that speak the MySQL protocol and use the MySQL driver, shown as their own type. */
export type MysqlFlavor = "mariadb" | "singlestore";
/** What the connection form offers and the app labels: a driver, or a MySQL flavor. */
export type ConnectionKind = Driver | MysqlFlavor;

export const DRIVER_OPTIONS: { id: Driver; label: string; defaultPort: number }[] = [
  { id: "mysql", label: "MySQL", defaultPort: 3306 },
  { id: "postgres", label: "PostgreSQL", defaultPort: 5432 },
  { id: "sqlite", label: "SQLite", defaultPort: 0 },
  { id: "mssql", label: "SQL Server", defaultPort: 1433 },
  { id: "redis", label: "Redis", defaultPort: 6379 },
  { id: "mongodb", label: "MongoDB", defaultPort: 27017 },
];

/** Redis and MongoDB aren't SQL: no tables, SQL, imports, or backups. */
export function isSqlDriver(driver: Driver) {
  return driver !== "redis" && driver !== "mongodb";
}

export function driverLabel(driver: Driver) {
  return DRIVER_OPTIONS.find((option) => option.id === driver)?.label ?? driver;
}

export const KIND_OPTIONS: { id: ConnectionKind; driver: Driver; flavor?: MysqlFlavor; label: string }[] = [
  { id: "mysql", driver: "mysql", label: "MySQL" },
  { id: "postgres", driver: "postgres", label: "PostgreSQL" },
  { id: "sqlite", driver: "sqlite", label: "SQLite" },
  { id: "mssql", driver: "mssql", label: "SQL Server" },
  { id: "redis", driver: "redis", label: "Redis" },
  { id: "mariadb", driver: "mysql", flavor: "mariadb", label: "MariaDB" },
  { id: "singlestore", driver: "mysql", flavor: "singlestore", label: "SingleStore" },
  { id: "mongodb", driver: "mongodb", label: "MongoDB" },
];

export function connectionKind(entry: { driver: Driver; flavor?: MysqlFlavor | null }): ConnectionKind {
  return entry.driver === "mysql" && entry.flavor ? entry.flavor : entry.driver;
}

/** "MariaDB" for a MariaDB connection, where driverLabel says "MySQL". */
export function connectionLabel(entry: { driver: Driver; flavor?: MysqlFlavor | null }) {
  const kind = connectionKind(entry);
  return KIND_OPTIONS.find((option) => option.id === kind)?.label ?? entry.driver;
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
  /** False while Recon waits on something outside the app, like a browser sign-in. */
  input: boolean;
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
  /** MySQL only: the server is MariaDB or SingleStore. */
  flavor?: MysqlFlavor;
  host: string;
  port: number;
  user: string;
  database: string;
  filePath: string;
  sslMode: SslMode;
  headerColor: string;
  savePassword: boolean;
  cleartextAuth?: boolean;
  passwordSource?: PasswordSource;
  awsRegion?: string;
  awsProfile?: string;
  passwordCommand?: string;
  sslCaPath?: string;
  sslCertPath?: string;
  sslKeyPath?: string;
  /** MongoDB only: a mongodb:// or mongodb+srv:// connection string, used instead of host and port. */
  uri?: string;
  /** MongoDB only: the database the user signs in against, "admin" when empty. */
  authDatabase?: string;
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
  s3Locations?: S3Location[];
  theme?: ThemePreference;
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
  multithreadedImport?: boolean;
  notifications?: NotificationMode;
  machineStats?: boolean;
  temperatureUnit?: TemperatureUnit;
  window?: WindowState;
}

/** "" leaves encryption to the bucket's default. */
export type S3Encryption = "" | "AES256" | "aws:kms";

/** A bucket, and optionally a folder in it, that backups and imports can use through the AWS CLI. */
export interface S3Location {
  id: string;
  name: string;
  /** Empty uses the AWS CLI's default credentials. */
  profile: string;
  /** Empty uses the profile's region. */
  region: string;
  bucket: string;
  /** A folder inside the bucket, ending in "/" when set. */
  prefix: string;
  /** For S3-compatible storage such as Cloudflare R2 or MinIO. */
  endpoint: string;
  encryption: S3Encryption;
  kmsKeyId: string;
}

export interface S3File {
  key: string;
  size: number;
  /** RFC 3339. */
  modified: string;
}

export interface S3Listing {
  prefix: string;
  folders: string[];
  files: S3File[];
  /** Only the first 1,000 entries are listed. */
  truncated: boolean;
}

/** Who the AWS CLI signs in as with a profile. */
export interface AwsIdentity {
  account: string;
  arn: string;
}

/** Where an import or restore reads from. */
export type TransferSource =
  | { kind: "file"; path: string }
  | { kind: "s3"; location: S3Location; key: string };

export type NotificationMode = "background" | "always" | "off";

export type TemperatureUnit = "fahrenheit" | "celsius";

export type ThemePreference = "system" | "light" | "dark";

export type ResolvedTheme = Exclude<ThemePreference, "system">;

export interface PreferencesPatch {
  theme?: ThemePreference;
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
  multithreadedImport?: boolean;
  notifications?: NotificationMode;
  machineStats?: boolean;
  temperatureUnit?: TemperatureUnit;
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

/** `contains` replaces every occurrence inside a value; `whole` replaces values that are exactly `find`. */
export type ReplaceMode = "contains" | "whole";

export interface ReplaceRequest {
  namespace: string;
  table: string;
  column: string;
  find: string;
  replace: string;
  mode: ReplaceMode;
  filter: WireNode | null;
  /** Minutes east of UTC. */
  utcOffset: number;
}

export interface ReplacePreview {
  count: number;
  samples: { before: string; after: string }[];
  /** The UPDATE with its values written out. */
  sql: string;
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

export interface ImportWorkerProgress {
  index: number;
  statements: number;
  totalStatements: number;
  state: "waiting" | "running" | "done";
}

export interface ImportProgress {
  transferId: string;
  statements: number;
  bytes: number;
  totalBytes: number;
  /**
   * `planning` while the file is split, `running`, `skipping` past statements that already ran, or
   * `reconnecting` after the connection dropped.
   */
  phase: "planning" | "running" | "skipping" | "reconnecting";
  /** Failed reconnect attempts so far, with the last one's error. */
  attempt: number;
  error: string | null;
  fileBytes: number;
  fileTotalBytes: number;
  /** 0 until the split has counted statements, and always for a run in file order. */
  totalStatements: number;
  /** Empty for a run in file order. */
  workers: ImportWorkerProgress[];
  /** Why a run that could have loaded in parallel runs in file order. Sent once, before it starts. */
  inOrder?: string;
}

/** Per-logical-core load and, when the machine exposes one, a Celsius temperature. */
export interface MachineStats {
  /** One number per logical core, 0–100, or empty when that reading is unavailable. */
  cores: number[];
  /** Degrees Celsius, or null when unavailable. */
  temperatureCelsius: number | null;
}

export interface ImportBackend {
  id: number;
  started: string | null;
}

/** Where an import or restore that lost its connection can pick up again. */
export interface ImportResumePoint {
  /** Statements to skip to run the uncertain ones again, or just to carry on when none are. */
  from: number;
  /** Statements to skip to go past the uncertain ones instead. Equal to `from` when none are. */
  to: number;
  line: number;
  fingerprint: string;
  backend: ImportBackend | null;
  likelySaved: boolean;
  /** The run loaded the file over several connections, so a resumed run must too. */
  parallel: boolean;
  splitTables: boolean;
  /** For a parallel run, the statements that finished, as half-open index ranges. */
  done: [number, number][];
  /** For a parallel run, the statements between `from` and `to` that may or may not have been saved. */
  uncertain: number[];
}

export interface ImportResume {
  skip: number;
  line: number;
  fingerprint: string;
  backend: ImportBackend | null;
  parallel: boolean;
  splitTables: boolean;
  done: [number, number][];
  /** The uncertain statements to treat as saved, when skipping past them. */
  skipped: number[];
}

export interface ImportFailure {
  message: string;
  resume: ImportResumePoint | null;
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

/** A Redis key or value. `hex` holds the exact bytes when they aren't UTF-8, and `text` is then only for display. */
export interface RedisBytes {
  text: string;
  hex?: string;
}

export type RedisKind = "string" | "hash" | "list" | "set" | "zset" | "stream" | (string & {});

export interface RedisDatabase {
  index: number;
  keys: number;
}

export interface RedisInfo {
  serverVersion: string;
  db: number;
  databases: RedisDatabase[];
}

export interface RedisKeyEntry {
  key: RedisBytes;
  kind: RedisKind;
}

export interface RedisScanPage {
  cursor: string | null;
  keys: RedisKeyEntry[];
}

export interface RedisKeyInfo {
  kind: RedisKind;
  /** Milliseconds left, -1 for no expiry, or -2 once the key is gone. */
  ttlMs: number;
  length: number | null;
  memory: number | null;
  encoding: string | null;
}

export interface RedisFieldValue {
  field: RedisBytes;
  value: RedisBytes;
}

export type RedisValuePage =
  | { kind: "string"; value: RedisBytes; size: number; truncated: boolean }
  | { kind: "hash"; entries: RedisFieldValue[]; cursor: string | null }
  | { kind: "list"; items: { index: number; value: RedisBytes }[]; cursor: string | null }
  | { kind: "set"; members: RedisBytes[]; cursor: string | null }
  | { kind: "zset"; members: { member: RedisBytes; score: number }[]; cursor: string | null }
  | { kind: "stream"; entries: { id: string; fields: RedisFieldValue[] }[]; cursor: string | null }
  | { kind: "other"; typeName: string };

export type RedisEdit =
  | { op: "setString"; key: RedisBytes; value: RedisBytes }
  | { op: "hashSet"; key: RedisBytes; field: RedisBytes; value: RedisBytes; original: RedisBytes | null }
  | { op: "hashDelete"; key: RedisBytes; fields: RedisBytes[] }
  | { op: "listSet"; key: RedisBytes; index: number; value: RedisBytes }
  | { op: "listPush"; key: RedisBytes; value: RedisBytes; head: boolean }
  | { op: "listRemove"; key: RedisBytes; indexes: number[] }
  | { op: "setAdd"; key: RedisBytes; member: RedisBytes; original: RedisBytes | null }
  | { op: "setRemove"; key: RedisBytes; members: RedisBytes[] }
  | { op: "zsetAdd"; key: RedisBytes; member: RedisBytes; score: number; original: RedisBytes | null }
  | { op: "zsetRemove"; key: RedisBytes; members: RedisBytes[] }
  | { op: "streamAdd"; key: RedisBytes; fields: [RedisBytes, RedisBytes][] }
  | { op: "streamDelete"; key: RedisBytes; ids: string[] }
  | { op: "rename"; key: RedisBytes; to: RedisBytes; overwrite: boolean }
  | { op: "expire"; key: RedisBytes; seconds: number | null }
  | { op: "delete"; keys: RedisBytes[] };

export interface RedisNewKey {
  key: RedisBytes;
  kind: RedisKind;
  field: RedisBytes | null;
  value: RedisBytes;
  score: number | null;
  ttlSeconds: number | null;
}

export type RedisReply =
  | { type: "nil" }
  | { type: "int"; value: number }
  | { type: "bulk"; value: RedisBytes }
  | { type: "status"; value: string }
  | { type: "error"; value: string }
  | { type: "array"; value: RedisReply[] }
  | { type: "set"; value: RedisReply[] }
  | { type: "map"; value: [RedisReply, RedisReply][] }
  | { type: "double"; value: number | null }
  | { type: "bool"; value: boolean }
  | { type: "big"; value: string };

export interface RedisCommandResult {
  reply: RedisReply;
  durationMs: number;
}

export interface MongoDatabase {
  name: string;
  sizeOnDisk: number;
  empty: boolean;
}

export interface MongoInfo {
  serverVersion: string;
  database: string;
  databases: MongoDatabase[];
}

export interface MongoCollection {
  name: string;
  kind: "collection" | "view" | "timeseries";
  count: number | null;
}

export type MongoKind =
  | "string"
  | "int"
  | "long"
  | "double"
  | "decimal"
  | "bool"
  | "null"
  | "objectId"
  | "date"
  | "object"
  | "array"
  | "binary"
  | "regex"
  | "timestamp"
  | "other";

export interface MongoField {
  name: string;
  value: null | boolean | number | string;
  kind: MongoKind;
}

/** A document: its `_id` as relaxed Extended JSON, its mongosh-style text, and its fields for the grid. */
export interface MongoDocument {
  id: unknown;
  text: string;
  fields: MongoField[];
}

export interface MongoFindPage {
  documents: MongoDocument[];
  total: number | null;
  durationMs: number;
}

export interface MongoIndex {
  name: string;
  keys: string;
  unique: boolean;
  sparse: boolean;
  hidden: boolean;
  ttlSeconds: number | null;
  partial: string | null;
  text: string;
}

export interface MongoConsoleResult {
  output: string;
  error: boolean;
  durationMs: number;
  changed: boolean;
  database: string | null;
}
