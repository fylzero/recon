import { onUnmounted, ref } from "vue";

interface DragReorderOptions {
  ids: () => string[];
  selector: string;
  datasetKey: string;
  bodyClass: string;
  commit: (ids: string[]) => Promise<void>;
  isBefore?: (element: HTMLElement, event: PointerEvent) => boolean;
}

function midpointBefore(element: HTMLElement, event: PointerEvent) {
  const rect = element.getBoundingClientRect();
  return event.clientY < rect.top + rect.height / 2;
}

export function useDragReorder(options: DragReorderOptions) {
  const draggingId = ref<string | null>(null);
  const draftIds = ref<string[] | null>(null);

  function moveTo(targetId: string, before: boolean) {
    const dragging = draggingId.value;
    if (!dragging || dragging === targetId) {
      return;
    }
    const ids = [...(draftIds.value ?? options.ids())];
    const from = ids.indexOf(dragging);
    if (from === -1) {
      return;
    }
    ids.splice(from, 1);
    let to = ids.indexOf(targetId);
    if (to === -1) {
      return;
    }
    if (!before) {
      to += 1;
    }
    ids.splice(to, 0, dragging);
    if (ids.join("\0") !== (draftIds.value ?? []).join("\0")) {
      draftIds.value = ids;
    }
  }

  function onMove(event: PointerEvent) {
    if (!draggingId.value) {
      return;
    }
    const node = document.elementFromPoint(event.clientX, event.clientY);
    const element = node instanceof Element ? node.closest(options.selector) : null;
    if (!(element instanceof HTMLElement)) {
      return;
    }
    const id = element.dataset[options.datasetKey];
    if (!id || !options.ids().includes(id)) {
      return;
    }
    moveTo(id, (options.isBefore ?? midpointBefore)(element, event));
  }

  function detach() {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", finish);
    window.removeEventListener("pointercancel", finish);
    document.body.classList.remove(options.bodyClass);
  }

  async function finish() {
    detach();
    const ids = draftIds.value;
    draggingId.value = null;
    draftIds.value = null;
    if (!ids || ids.join("\0") === options.ids().join("\0")) {
      return;
    }
    try {
      await options.commit(ids);
    } catch (err) {
      window.alert(String(err));
    }
  }

  function start(event: PointerEvent, id: string) {
    if (event.button !== 0 || options.ids().length < 2) {
      return;
    }
    event.preventDefault();
    draggingId.value = id;
    draftIds.value = options.ids();
    document.body.classList.add(options.bodyClass);
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", finish);
    window.addEventListener("pointercancel", finish);
  }

  function ordered<T extends { id: string }>(items: T[]) {
    if (!draftIds.value) {
      return items;
    }
    const byId = new Map(items.map((item) => [item.id, item]));
    return draftIds.value.flatMap((id) => {
      const item = byId.get(id);
      return item ? [item] : [];
    });
  }

  onUnmounted(detach);

  return { draggingId, start, ordered };
}

export function alphabeticalIds<T extends { id: string; name: string }>(items: T[]) {
  return [...items]
    .sort((left, right) =>
      left.name.localeCompare(right.name, undefined, { sensitivity: "base", numeric: true }),
    )
    .map((item) => item.id);
}
