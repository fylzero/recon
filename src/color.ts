export const DEFAULT_HEADER_COLOR = "#16323c";

export function contrastingText(color: string) {
  const hex = color.replace("#", "");
  const normalized =
    hex.length === 3
      ? hex
          .split("")
          .map((part) => part + part)
          .join("")
      : hex;
  if (normalized.length < 6) {
    return "#e8edf5";
  }
  const red = Number.parseInt(normalized.slice(0, 2), 16);
  const green = Number.parseInt(normalized.slice(2, 4), 16);
  const blue = Number.parseInt(normalized.slice(4, 6), 16);
  const luma = red * 0.299 + green * 0.587 + blue * 0.114;
  return luma > 150 ? "#14161b" : "#e8edf5";
}
