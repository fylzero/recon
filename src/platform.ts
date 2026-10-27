export const isMac =
  typeof navigator === "undefined" || /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

export const isWindows = typeof navigator !== "undefined" && /Win/.test(navigator.platform || navigator.userAgent);

export const altKeyName = isMac ? "Option" : "Alt";

export const fileManagerName = isMac ? "Finder" : isWindows ? "File Explorer" : "File Manager";

export const secretStoreName = isMac ? "Keychain" : isWindows ? "Credential Manager" : "keyring";

export const secretStoreFullName = isMac
  ? "macOS Keychain"
  : isWindows
    ? "Windows Credential Manager"
    : "system keyring";

/** How a password command reads the RECON_DB_* variables it's given. */
export const passwordCommandShell = isWindows
  ? "Runs in cmd each time Recon connects, and what it prints is the password. %RECON_DB_HOST%, %RECON_DB_PORT%, and %RECON_DB_USER% are set for it."
  : "Runs in your shell each time Recon connects, and what it prints is the password. RECON_DB_HOST, RECON_DB_PORT, and RECON_DB_USER are set for it.";

/** The last part of a file path, which on Windows may use backslashes. */
export function baseName(path: string) {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

export const systemNotificationsTarget = isMac
  ? "to macOS Notification Center"
  : "as desktop notifications";

/** The aria-keyshortcuts name for the key that ⌘ maps to. */
export const ariaModifier = isMac ? "Meta" : "Control";

const MODIFIERS: [glyph: string, name: string][] = [
  ["⌃", "Ctrl"],
  ["⌘", "Ctrl"],
  ["⇧", "Shift"],
  ["⌥", "Alt"],
];

const KEY_NAMES: Record<string, string> = { "↵": "Enter", "⌫": "Backspace" };

/** Writes a macOS shortcut such as ⇧⌘F the way other platforms do, as Ctrl+Shift+F. */
export function shortcut(keys: string) {
  if (isMac) {
    return keys;
  }
  const names = new Set(MODIFIERS.filter(([glyph]) => keys.includes(glyph)).map(([, name]) => name));
  const key = [...keys].filter((char) => !MODIFIERS.some(([glyph]) => glyph === char)).join("");
  return [...names, ...(key ? [KEY_NAMES[key] ?? key] : [])].join("+");
}
