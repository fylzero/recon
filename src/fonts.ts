export const CODE_FONT_FALLBACK =
  "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace";

export const FONT_OPTIONS: { id: string; label: string; stack: string }[] = [
  {
    id: "jetbrains",
    label: "JetBrains Mono",
    stack: `"JetBrains Mono", ${CODE_FONT_FALLBACK}`,
  },
  { id: "system", label: "System mono", stack: CODE_FONT_FALLBACK },
  { id: "sf-mono", label: "SF Mono", stack: `"SF Mono", ${CODE_FONT_FALLBACK}` },
  { id: "menlo", label: "Menlo", stack: `Menlo, ${CODE_FONT_FALLBACK}` },
  { id: "monaco", label: "Monaco", stack: `Monaco, ${CODE_FONT_FALLBACK}` },
  {
    id: "courier",
    label: "Courier New",
    stack: `"Courier New", Courier, ${CODE_FONT_FALLBACK}`,
  },
];

export const CUSTOM_FONT_ID = "custom";
export const DEFAULT_CODE_FONT = "jetbrains";
export const DEFAULT_EDITOR_FONT_SIZE = 13;
export const DEFAULT_GRID_FONT_SIZE = 12;
export const FONT_SIZE_MIN = 9;
export const FONT_SIZE_MAX = 22;

const FONT_ALIASES: Record<string, string> = {
  jetbrains: "jetbrains",
  "jetbrains mono": "jetbrains",
  system: "system",
  "system mono": "system",
  default: "system",
  "ui-monospace": "system",
  "sf-mono": "sf-mono",
  "sf mono": "sf-mono",
  sfmono: "sf-mono",
  menlo: "menlo",
  monaco: "monaco",
  courier: "courier",
  "courier new": "courier",
};

export function sanitizeFontFamily(value: string) {
  const trimmed = value.trim();
  if (
    !trimmed ||
    trimmed.length > 80 ||
    /[/\\;{}]/.test(trimmed) ||
    trimmed.split("").some((char) => char.charCodeAt(0) < 32)
  ) {
    return DEFAULT_CODE_FONT;
  }
  return FONT_ALIASES[trimmed.toLowerCase()] ?? trimmed.replace(/['"]/g, "");
}

export function isPresetFont(value: string) {
  return FONT_OPTIONS.some((option) => option.id === value);
}

export function resolveFontStack(value: string) {
  const family = sanitizeFontFamily(value);
  const preset = FONT_OPTIONS.find((option) => option.id === family);
  if (preset) {
    return preset.stack;
  }
  const quoted = /[^\w-]/.test(family) ? `"${family}"` : family;
  return `${quoted}, ${CODE_FONT_FALLBACK}`;
}

export function clampFontSize(size: number, fallback: number) {
  if (!Number.isFinite(size)) {
    return fallback;
  }
  const snapped = Math.round(size * 2) / 2;
  return Math.min(FONT_SIZE_MAX, Math.max(FONT_SIZE_MIN, snapped));
}

export function formatFontSize(size: number) {
  return `${Number.isInteger(size) ? size : size.toFixed(1)}px`;
}

export function fontFaceName(stack: string) {
  return stack.split(",")[0]?.trim().replace(/^["']|["']$/g, "") ?? "";
}
