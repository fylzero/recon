import { invoke } from "@tauri-apps/api/core";
import type {
  AppData,
  BackupInfo,
  BrowseRequest,
  BrowseResult,
  ConnectionEntry,
  ConnectionGroup,
  DropOptions,
  ExportRequest,
  ExportResult,
  ImportFileOptions,
  ImportPreview,
  ImportResult,
  ImportRowsRequest,
  ImportRowsResult,
  NamespaceList,
  PreferencesPatch,
  QueryLogEntry,
  ResolvedSshHost,
  RowValues,
  SavedQuery,
  SaveRequest,
  SchemaColumn,
  SchemaDiagram,
  SessionInfo,
  StatementResult,
  TableInfo,
  TabularOptions,
  TableStructure,
  TruncateOptions,
  WindowState,
} from "./types";

export function getState() {
  return invoke<AppData>("get_state");
}

export function createGroup(name: string, headerColor?: string) {
  return invoke<ConnectionGroup>("create_group", { name, headerColor: headerColor ?? null });
}

export function updateGroup(groupId: string, name: string, headerColor?: string) {
  return invoke<void>("update_group", { groupId, name, headerColor: headerColor ?? null });
}

export function deleteGroup(groupId: string) {
  return invoke<void>("delete_group", { groupId });
}

export function toggleGroup(groupId: string) {
  return invoke<boolean>("toggle_group", { groupId });
}

export function setAllGroupsExpanded(expanded: boolean) {
  return invoke<void>("set_all_groups_expanded", { expanded });
}

export function reorderDashboard(ids: string[]) {
  return invoke<void>("reorder_dashboard", { ids });
}

export function saveConnection(
  groupId: string | null,
  connection: ConnectionEntry,
  password: string | null,
  sshSecret: string | null = null,
  sshPassword: string | null = null,
) {
  return invoke<ConnectionEntry>("save_connection", {
    groupId,
    connection,
    password,
    sshSecret,
    sshPassword,
  });
}

export function removeConnection(connectionId: string) {
  return invoke<void>("remove_connection", { connectionId });
}

export function reorderConnections(groupId: string | null, connectionIds: string[]) {
  return invoke<void>("reorder_connections", { groupId, connectionIds });
}

export function moveConnection(
  connectionId: string,
  groupId: string | null,
  connectionIds: string[],
) {
  return invoke<void>("move_connection", { connectionId, groupId, connectionIds });
}

export function saveQuery(query: SavedQuery) {
  return invoke<SavedQuery>("save_query", { query });
}

export function deleteSavedQuery(queryId: string) {
  return invoke<void>("delete_saved_query", { queryId });
}

export function hasSavedPassword(connectionId: string) {
  return invoke<boolean>("has_saved_password", { connectionId });
}

export function listSshKeys() {
  return invoke<string[]>("list_ssh_keys");
}

export function hasSavedSshSecret(connectionId: string) {
  return invoke<boolean>("has_saved_ssh_secret", { connectionId });
}

export function hasSavedSshPassword(connectionId: string) {
  return invoke<boolean>("has_saved_ssh_password", { connectionId });
}

export function listSshHosts() {
  return invoke<string[]>("list_ssh_hosts");
}

export function resolveSshHost(host: string) {
  return invoke<ResolvedSshHost | null>("resolve_ssh_host", { host });
}

export function answerPrompt(id: string, answer: string | null) {
  return invoke<void>("answer_prompt", { id, answer });
}

export function updatePreferences(patch: PreferencesPatch) {
  return invoke<AppData>("update_preferences", { patch });
}

export function replaceAppData(data: AppData) {
  return invoke<AppData>("replace_app_data", { data });
}

export function getWindowState() {
  return invoke<WindowState>("get_window_state");
}

export function updateWindowState(window: WindowState) {
  return invoke<WindowState>("update_window_state", { window });
}

export function writeTextFile(path: string, contents: string) {
  return invoke<void>("write_text_file", { path, contents });
}

export function readTextFile(path: string) {
  return invoke<string>("read_text_file", { path });
}

export function settingsFilePath() {
  return invoke<string>("settings_file_path");
}

export function revealSettingsFile() {
  return invoke<void>("reveal_settings_file");
}

export function revealPath(path: string) {
  return invoke<void>("reveal_path", { path });
}

export function queryHistory(connectionId: string) {
  return invoke<QueryLogEntry[]>("query_history", { connectionId });
}

export function queryHistoryPaused(connectionId: string) {
  return invoke<boolean>("query_history_paused", { connectionId });
}

export function setQueryHistoryPaused(connectionId: string, paused: boolean) {
  return invoke<void>("set_query_history_paused", { connectionId, paused });
}

export function clearQueryHistory(connectionId: string) {
  return invoke<void>("clear_query_history", { connectionId });
}

export function testConnection(
  connection: ConnectionEntry,
  password: string | null,
  sshSecret: string | null = null,
  sshPassword: string | null = null,
) {
  return invoke<string>("test_connection", { connection, password, sshSecret, sshPassword });
}

export function createSqliteDatabase(path: string) {
  return invoke<void>("create_sqlite_database", { path });
}

export function connect(
  connectionId: string,
  password: string | null,
  sessionId: string | null = null,
  namespace: string | null = null,
) {
  return invoke<SessionInfo>("connect", { connectionId, password, sessionId, namespace });
}

export function reconnect(connectionId: string, password: string | null = null) {
  return invoke<SessionInfo>("reconnect", { connectionId, password });
}

export function disconnect(connectionId: string) {
  return invoke<void>("disconnect", { connectionId });
}

export function listDatabases(connectionId: string) {
  return invoke<NamespaceList>("list_databases", { connectionId });
}

export function setDatabase(connectionId: string, namespace: string) {
  return invoke<void>("set_database", { connectionId, namespace });
}

export function createDatabase(connectionId: string, namespace: string) {
  return invoke<void>("create_database", { connectionId, namespace });
}

export function dropDatabase(connectionId: string, namespace: string) {
  return invoke<void>("drop_database", { connectionId, namespace });
}

export function renameDatabase(connectionId: string, from: string, to: string) {
  return invoke<void>("rename_database", { connectionId, from, to });
}

export function listTables(connectionId: string, namespace: string) {
  return invoke<TableInfo[]>("list_tables", { connectionId, namespace });
}

export function truncateTables(connectionId: string, namespace: string, tables: string[], options: TruncateOptions) {
  return invoke<void>("truncate_tables", { connectionId, namespace, tables, options });
}

export function dropTables(connectionId: string, namespace: string, tables: string[], options: DropOptions) {
  return invoke<void>("drop_tables", { connectionId, namespace, tables, options });
}

export function tableStructure(connectionId: string, namespace: string, table: string) {
  return invoke<TableStructure>("table_structure", { connectionId, namespace, table });
}

export function schemaColumns(connectionId: string, namespace: string) {
  return invoke<SchemaColumn[]>("schema_columns", { connectionId, namespace });
}

export function schemaDiagram(connectionId: string, namespace: string) {
  return invoke<SchemaDiagram>("schema_diagram", { connectionId, namespace });
}

/** The distinct values in each type column, in the same order as `columns`. */
export function morphTypes(connectionId: string, namespace: string, columns: SchemaColumn[]) {
  return invoke<string[][]>("morph_types", { connectionId, namespace, columns });
}

export function browseTable(connectionId: string, request: BrowseRequest) {
  return invoke<BrowseResult>("browse_table", { connectionId, request });
}

/** Resolves to null when counting takes longer than `timeoutMs`. */
export function countRows(connectionId: string, request: BrowseRequest, timeoutMs: number | null) {
  return invoke<number | null>("count_rows", { connectionId, request, timeoutMs });
}

export function cancelBrowse(connectionId: string, requestId: string) {
  return invoke<void>("cancel_browse", { connectionId, requestId });
}

export function previewBrowseSql(connectionId: string, request: BrowseRequest) {
  return invoke<string>("preview_browse_sql", { connectionId, request });
}

export function distinctValues(
  connectionId: string,
  namespace: string,
  table: string,
  column: string,
  search: string,
  limit = 50,
) {
  return invoke<string[]>("distinct_values", { connectionId, namespace, table, column, search, limit });
}

export function saveTableChanges(connectionId: string, requests: SaveRequest[]) {
  return invoke<number>("save_table_changes", { connectionId, requests });
}

export function runQuery(connectionId: string, statements: string[]) {
  return invoke<StatementResult[]>("run_query", { connectionId, statements });
}

export function fetchRows(resultId: string, offset: number, limit: number) {
  return invoke<RowValues[]>("fetch_rows", { resultId, offset, limit });
}

export function closeResults(resultIds: string[]) {
  return invoke<void>("close_results", { resultIds });
}

export function cancelQuery(connectionId: string) {
  return invoke<void>("cancel_query", { connectionId });
}

export function exportSql(connectionId: string, transferId: string, request: ExportRequest) {
  return invoke<ExportResult>("export_sql", { connectionId, transferId, request });
}

export function exportBrowse(
  connectionId: string,
  transferId: string,
  request: BrowseRequest,
  pageOnly: boolean,
  options: TabularOptions,
  path: string,
) {
  return invoke<ExportResult>("export_browse", { connectionId, transferId, request, pageOnly, options, path });
}

export function exportResult(resultId: string, columns: string[], options: TabularOptions, path: string) {
  return invoke<ExportResult>("export_result", { resultId, columns, options, path });
}

export function previewImport(connectionId: string, path: string, options: ImportFileOptions) {
  return invoke<ImportPreview>("preview_import", { connectionId, path, options });
}

export function importRows(connectionId: string, transferId: string, request: ImportRowsRequest) {
  return invoke<ImportRowsResult>("import_rows", { connectionId, transferId, request });
}

export function importSql(connectionId: string, transferId: string, namespace: string, path: string) {
  return invoke<ImportResult>("import_sql", { connectionId, transferId, namespace, path });
}

export function backupDatabase(connectionId: string, transferId: string, namespace: string, path: string) {
  return invoke<ExportResult>("backup_database", { connectionId, transferId, namespace, path });
}

export function readBackupInfo(path: string) {
  return invoke<BackupInfo>("read_backup_info", { path });
}

export function restoreDatabase(connectionId: string, transferId: string, namespace: string, path: string) {
  return invoke<ImportResult>("restore_database", { connectionId, transferId, namespace, path });
}

export function cancelTransfer(transferId: string) {
  return invoke<void>("cancel_transfer", { transferId });
}
