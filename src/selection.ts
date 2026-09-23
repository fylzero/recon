export function rangeIds(siblings: string[], anchorId: string, targetId: string): string[] {
  const start = siblings.indexOf(anchorId);
  const end = siblings.indexOf(targetId);
  if (start === -1 || end === -1) {
    return [targetId];
  }
  return siblings.slice(Math.min(start, end), Math.max(start, end) + 1);
}
