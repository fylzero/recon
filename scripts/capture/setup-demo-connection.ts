#!/usr/bin/env bun
/**
 * Sets up an isolated Recon app data directory with a demo SQLite connection.
 * This ensures the capture script doesn't interfere with the user's real connections.
 * 
 * The script creates a settings.json file in the isolated app data directory
 * with a single connection pointing to the demo database.
 */

import * as fs from "fs";
import * as path from "path";

const TEMP_DIR = process.env.RECON_DEMO_DB_DIR || "/tmp/recon-demo";
const DB_PATH = path.join(TEMP_DIR, "demo.db");

// The isolated HOME directory
const ISOLATED_HOME = path.join(TEMP_DIR, "home");

// Tauri app data path: ~/Library/Application Support/com.recon.app
const APP_DATA_DIR = path.join(ISOLATED_HOME, "Library", "Application Support", "com.recon.app");

// Ensure directories exist
fs.mkdirSync(APP_DATA_DIR, { recursive: true });

// Fake connections for the dashboard. Only "Demo Store" (SQLite) is opened by
// the capture; the others exist to show groups and every driver icon, and
// point at made-up .internal/.test hosts. Every id contains "demo-connection"
// so localstorage.py purge-demo removes any webview state they leave behind.
type Kind = { driver: string; flavor?: string };
function conn(
  id: string,
  name: string,
  kind: Kind,
  fields: Partial<{ host: string; port: number; user: string; database: string; filePath: string; headerColor: string; sshHost: string }>,
) {
  return {
    id: `demo-connection${id ? `-${id}` : ""}`,
    name,
    driver: kind.driver,
    ...(kind.flavor ? { flavor: kind.flavor } : {}),
    host: fields.host ?? "",
    port: fields.port ?? 0,
    user: fields.user ?? "",
    database: fields.database ?? "",
    filePath: fields.filePath ?? "",
    sslMode: "prefer",
    headerColor: fields.headerColor ?? "",
    savePassword: kind.driver === "sqlite",
    cleartextAuth: false,
    passwordSource: "password",
    awsRegion: "",
    awsProfile: "",
    passwordCommand: "",
    sslCaPath: "",
    sslCertPath: "",
    sslKeyPath: "",
    ssh: {
      enabled: Boolean(fields.sshHost),
      host: fields.sshHost ?? "",
      port: 22,
      user: fields.sshHost ? "deploy" : "",
      auth: "key",
      keyPath: fields.sshHost ? "~/.ssh/id_ed25519" : "",
      alsoPassword: false,
    },
  };
}

const settings = {
  groups: [
    {
      id: "demo-connection-group-shop",
      name: "Juniper Goods",
      expanded: true,
      headerColor: "#0f766e",
      connections: [
        conn("shop-prod", "Production", { driver: "mysql" }, {
          host: "db-primary.juniper.internal", port: 3306, user: "app", database: "juniper_shop", sshHost: "bastion.juniper.internal",
        }),
        conn("shop-staging", "Staging", { driver: "postgres" }, {
          host: "staging-db.juniper.test", port: 5432, user: "app", database: "juniper_staging",
        }),
        conn("shop-sessions", "Sessions & cache", { driver: "redis" }, {
          host: "cache.juniper.internal", port: 6379, database: "0",
        }),
        conn("shop-catalog", "Product catalog", { driver: "mongodb" }, {
          host: "catalog.juniper.internal", port: 27017, user: "catalog", database: "catalog",
        }),
      ],
    },
    {
      id: "demo-connection-group-analytics",
      name: "Analytics",
      expanded: true,
      headerColor: "#6d28d9",
      connections: [
        conn("warehouse", "Event warehouse", { driver: "mysql", flavor: "singlestore" }, {
          host: "warehouse.analytics.internal", port: 3306, user: "analyst", database: "events",
        }),
        conn("reporting", "Reporting replica", { driver: "mysql", flavor: "mariadb" }, {
          host: "reports.analytics.internal", port: 3306, user: "readonly", database: "reports",
        }),
        conn("billing", "Billing", { driver: "mssql" }, {
          host: "billing-sql.finance.internal", port: 1433, user: "billing_ro", database: "Billing",
        }),
      ],
    },
  ],
  connections: [
    conn("", "Demo Store", { driver: "sqlite" }, { filePath: DB_PATH }),
  ],
  dashboardOrder: ["demo-connection", "demo-connection-group-shop", "demo-connection-group-analytics"],
  savedQueries: [],
  theme: "dark",
  editorFontFamily: "jetbrains",
  editorFontSize: 14,
  gridFontFamily: "jetbrains",
  gridFontSize: 13,
  listFontFamily: "ui",
  listFontSize: 12.5,
  pageSize: 300,
  queryRowLimit: 10000,
  sidebarWidth: 260,
  maxAutoColumnWidth: 480,
  autoApplyFilters: true,
  notifications: "background",
  window: {
    x: 100,
    y: 100,
    width: 1440,
    height: 900,
    maximized: false
  }
};

const settingsPath = path.join(APP_DATA_DIR, "settings.json");
fs.writeFileSync(settingsPath, JSON.stringify(settings, null, 2));

console.log(`✓ Demo connection configured in isolated app data`);
console.log(`  App data: ${APP_DATA_DIR}`);
console.log(`  Settings: ${settingsPath}`);
console.log(`  Connections: "${settings.connections[0].name}" → ${DB_PATH}, plus ${settings.groups.length} groups of fake servers`);
console.log("");
console.log(`To use this isolated environment, run the app with:`);
console.log(`  HOME="${ISOLATED_HOME}" /Applications/Recon.app/Contents/MacOS/Recon`);
