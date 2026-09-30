import { computed, ref, watch } from "vue";
import { cellText } from "../cells";
import type { CellPosition } from "../components/DataGrid.vue";
import type { RowValues } from "../types";

/** Past this many matches the grid stops counting, so huge results stay responsive. */
export const MAX_FIND_MATCHES = 10_000;

export type GridFind = ReturnType<typeof useGridFind>;

/** Searches the rows already loaded into a grid, cell by cell, as the text each cell shows. */
export function useGridFind(rows: () => (RowValues | undefined)[]) {
  const open = ref(false);
  const query = ref("");
  const index = ref(0);
  /** Bumped to ask the find input to take focus and select its text. */
  const focusRequest = ref(0);
  let anchor: CellPosition | null = null;

  const needle = computed(() => (open.value ? query.value.toLowerCase() : ""));

  const result = computed(() => {
    const list: CellPosition[] = [];
    let loaded = 0;
    const source = rows();
    const text = needle.value;
    for (let row = 0; row < source.length; row += 1) {
      const values = source[row];
      if (!values) {
        continue;
      }
      loaded += 1;
      if (!text || list.length >= MAX_FIND_MATCHES) {
        continue;
      }
      for (let col = 0; col < values.length && list.length < MAX_FIND_MATCHES; col += 1) {
        if (cellText(values[col] ?? null).toLowerCase().includes(text)) {
          list.push({ row, col });
        }
      }
    }
    return { list, loaded, total: source.length };
  });

  const matches = computed(() => result.value.list);
  const capped = computed(() => matches.value.length >= MAX_FIND_MATCHES);
  /** Whether some rows in the grid were not loaded yet, so find did not search them. */
  const partial = computed(() => result.value.loaded < result.value.total);

  const byRow = computed(() => {
    const cells = new Map<number, Set<number>>();
    for (const match of matches.value) {
      const set = cells.get(match.row);
      if (set) {
        set.add(match.col);
      } else {
        cells.set(match.row, new Set([match.col]));
      }
    }
    return cells;
  });

  const current = computed<CellPosition | null>(() => matches.value[index.value] ?? null);

  // New matches keep the place: the current match, or the first one after it.
  watch(matches, (list) => {
    if (!anchor) {
      index.value = 0;
      return;
    }
    const { row, col } = anchor;
    const next = list.findIndex((match) => match.row > row || (match.row === row && match.col >= col));
    index.value = next === -1 ? 0 : next;
  });

  watch(current, (match) => {
    if (match) {
      anchor = match;
    }
  });

  function show(from?: CellPosition | null) {
    if (!open.value && from) {
      anchor = from;
    }
    open.value = true;
    focusRequest.value += 1;
  }

  function close() {
    open.value = false;
  }

  function step(delta: 1 | -1) {
    const count = matches.value.length;
    if (!count) {
      return;
    }
    index.value = (index.value + delta + count) % count;
  }

  /** Searches for a cell's text, keeping focus where it is. */
  function search(text: string, from: CellPosition | null) {
    anchor = from;
    query.value = text;
    open.value = true;
  }

  return {
    open,
    query,
    index,
    focusRequest,
    matches,
    byRow,
    current,
    capped,
    partial,
    show,
    close,
    step,
    search,
  };
}
