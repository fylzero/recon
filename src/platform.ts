export const isMac =
  typeof navigator === "undefined" || /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

export const altKeyName = isMac ? "Option" : "Alt";

export const fileManagerName = isMac ? "Finder" : "File Manager";

export const secretStoreName = isMac ? "Keychain" : "keyring";

export const secretStoreFullName = isMac ? "macOS Keychain" : "system keyring";

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
