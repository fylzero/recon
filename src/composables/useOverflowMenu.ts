import { computed, onMounted, onUnmounted, ref } from "vue";

const openMenuId = ref<string | null>(null);
let listenerCount = 0;

function onDocumentPointerDown(event: PointerEvent) {
  const target = event.target;
  if (target instanceof Element && target.closest(".overflow-menu")) {
    return;
  }
  openMenuId.value = null;
}

function onDocumentKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    openMenuId.value = null;
  }
}

export function useOverflowMenu(id: () => string) {
  const isOpen = computed(() => openMenuId.value === id());

  function toggle() {
    const key = id();
    openMenuId.value = openMenuId.value === key ? null : key;
  }

  function close() {
    if (openMenuId.value === id()) {
      openMenuId.value = null;
    }
  }

  onMounted(() => {
    if (listenerCount === 0) {
      document.addEventListener("pointerdown", onDocumentPointerDown);
      document.addEventListener("keydown", onDocumentKeydown);
    }
    listenerCount += 1;
  });

  onUnmounted(() => {
    listenerCount -= 1;
    if (listenerCount === 0) {
      document.removeEventListener("pointerdown", onDocumentPointerDown);
      document.removeEventListener("keydown", onDocumentKeydown);
    }
    close();
  });

  return { isOpen, toggle, close };
}
