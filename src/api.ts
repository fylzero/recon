import { invoke } from "@tauri-apps/api/core";
import type {
  AppData,
  BrowseRequest,
  BrowseResult,
  ConnectionEntry,
  ConnectionGroup,
  NamespaceList,
  PreferencesPatch,
  QueryLogEntry,
  RowValues,
  SaveRequest,
  SchemaColumn,
  SessionInfo,
  StatementResult,
  TableInfo,
  TableStructure,
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

export function reorderGroups(groupIds: string[]) {
  return invoke<void>("reorder_groups", { groupIds });
}

export function saveConnection(
  groupId: string | null,
  connection: ConnectionEntry,
  password: string | null,
  sshSecret: string | null = null,
) {
  return invoke<ConnectionEntry>("save_connection", { groupId, connection, password, sshSecret });
}

export function removeConnection(connectionId: string) {
  return invoke<void>("remove_connection", { connectionId });
}

export function reorderConnections(groupId: string | null, connectionIds: string[]) {
  return invoke<void>("reorder_connections", { groupId, connectionIds });
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

export function queryHistory() {
  return invoke<QueryLogEntry[]>("query_history");
}

export function queryHistoryPaused() {
  return invoke<boolean>("query_history_paused");
}

export function setQueryHistoryPaused(paused: boolean) {
  return invoke<void>("set_query_history_paused", { paused });
}

export function clearQueryHistory() {
  return invoke<void>("clear_query_history");
}

export function testConnection(
  connection: ConnectionEntry,
  password: string | null,
  sshSecret: string | null = null,
) {
  return invoke<string>("test_connection", { connection, password, sshSecret });
}

export function createSqliteDatabase(path: string) {
  return invoke<void>("create_sqlite_database", { path });
}

export function connect(connectionId: string, password: string | null) {
  return invoke<SessionInfo>("connect", { connectionId, password });
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

export function listTables(connectionId: string, namespace: string) {
  return invoke<TableInfo[]>("list_tables", { connectionId, namespace });
}

export function tableStructure(connectionId: string, namespace: string, table: string) {
  return invoke<TableStructure>("table_structure", { connectionId, namespace, table });
}

export function schemaColumns(connectionId: string, namespace: string) {
  return invoke<SchemaColumn[]>("schema_columns", { connectionId, namespace });
}

export function browseTable(connectionId: string, request: BrowseRequest) {
  return invoke<BrowseResult>("browse_table", { connectionId, request });
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
