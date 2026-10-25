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

// Create the settings.json with a demo connection
const settings = {
  groups: [],
  connections: [
    {
      id: "demo-connection",
      name: "Demo Store",
      driver: "sqlite",
      host: "",
      port: 0,
      user: "",
      database: "",
      filePath: DB_PATH,
      sslMode: "prefer",
      headerColor: "#0891b2", // Cyan color for demo
      savePassword: true,
      cleartextAuth: false,
      passwordSource: "password",
      awsRegion: "",
      awsProfile: "",
      passwordCommand: "",
      sslCaPath: "",
      sslCertPath: "",
      sslKeyPath: "",
      ssh: {
        enabled: false,
        host: "",
        port: 22,
        user: "",
        auth: "password",
        keyPath: "",
        alsoPassword: false
      }
    }
  ],
  dashboardOrder: ["demo-connection"],
  savedQueries: [],
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
console.log(`  Connection: "${settings.connections[0].name}" → ${DB_PATH}`);
console.log("");
console.log(`To use this isolated environment, run the app with:`);
console.log(`  HOME="${ISOLATED_HOME}" /Applications/Recon.app/Contents/MacOS/Recon`);
