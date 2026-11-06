/**
 * The saved dashboard order without stale ids. Anything missing from it
 * follows, ungrouped items first, matching the backend's `dashboard_ids`.
 */
export function dashboardIds(order: string[], standaloneIds: string[], groupIds: string[]) {
  const known = new Set([...standaloneIds, ...groupIds]);
  const ids = new Set<string>();
  for (const id of [...order, ...standaloneIds, ...groupIds]) {
    if (known.has(id)) {
      ids.add(id);
    }
  }
  return [...ids];
}

export function alphabeticalIds<T extends { id: string; name: string }>(items: T[]) {
  return [...items]
    .sort((left, right) =>
      left.name.localeCompare(right.name, undefined, { sensitivity: "base", numeric: true }),
    )
    .map((item) => item.id);
}
