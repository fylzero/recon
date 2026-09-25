<script setup lang="ts">
import { computed, nextTick, onUnmounted, ref, watch } from "vue";
import { useOverflowMenu } from "../composables/useOverflowMenu";

const props = defineProps<{
  menuId: string;
  current: string;
  items: string[];
  label: string;
  creatable?: boolean;
  droppable?: boolean;
  renamable?: boolean;
}>();

const emit = defineEmits<{
  select: [name: string];
  open: [];
  create: [];
  drop: [name: string];
  rename: [name: string];
  copy: [name: string];
  openTab: [name: string];
}>();

const contextMenu = ref<{ name: string; x: number; y: number } | null>(null);

const { isOpen, toggle, close } = useOverflowMenu(() => `database-${props.menuId}`);
const query = ref("");
const searchInput = ref<HTMLInputElement | null>(null);
const menuEl = ref<HTMLElement | null>(null);
const listEl = ref<HTMLElement | null>(null);
const activeIndex = ref(0);
const moved = ref(false);

const filtered = computed(() => {
  const needle = query.value.trim().toLowerCase();
  return needle ? props.items.filter((item) => item.toLowerCase().includes(needle)) : props.items;
});

function currentIndex() {
  const index = props.items.indexOf(props.current);
  return index >= 0 ? index : 0;
}

function scrollIntoView(selector: string) {
  void nextTick(() => {
    const list = listEl.value;
    const item = list?.querySelector<HTMLElement>(selector);
    if (!list || !item) {
      return;
    }
    const listRect = list.getBoundingClientRect();
    const itemRect = item.getBoundingClientRect();
    if (itemRect.top < listRect.top) {
      list.scrollTop -= listRect.top - itemRect.top;
    } else if (itemRect.bottom > listRect.bottom) {
      list.scrollTop += itemRect.bottom - listRect.bottom;
    }
  });
}

function select(name: string) {
  close();
  if (name !== props.current) {
    emit("select", name);
  }
}

function create() {
  close();
  emit("create");
}

function onContextKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    event.stopImmediatePropagation();
    event.preventDefault();
    closeContextMenu();
  }
}

function openContextMenu(event: MouseEvent, name: string) {
  event.preventDefault();
  contextMenu.value = { name, x: event.clientX, y: event.clientY };
  document.addEventListener("keydown", onContextKeydown, true);
}

function closeContextMenu() {
  contextMenu.value = null;
  document.removeEventListener("keydown", onContextKeydown, true);
}

function onDropdownPointerDown(event: PointerEvent) {
  if (!(event.target instanceof Element && event.target.closest(".database-context-menu"))) {
    closeContextMenu();
  }
}

function runAction(action: "drop" | "rename" | "copy" | "openTab", name: string) {
  closeContextMenu();
  close();
  if (action === "drop") {
    emit("drop", name);
  } else if (action === "rename") {
    emit("rename", name);
  } else if (action === "copy") {
    emit("copy", name);
  } else {
    emit("openTab", name);
  }
}

function clearQuery() {
  query.value = "";
  searchInput.value?.focus();
}

function onHover(index: number) {
  moved.value = true;
  activeIndex.value = index;
}

function onSearchKeydown(event: KeyboardEvent) {
  const items = filtered.value;
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    if (!items.length) {
      return;
    }
    const delta = event.key === "ArrowDown" ? 1 : -1;
    moved.value = true;
    activeIndex.value = Math.min(items.length - 1, Math.max(0, activeIndex.value + delta));
    scrollIntoView(`[data-database-index="${activeIndex.value}"]`);
    return;
  }
  if (event.key === "Enter") {
    event.preventDefault();
    const item = items[activeIndex.value];
    if (!item || (!query.value.trim() && !moved.value)) {
      close();
      return;
    }
    select(item);
    return;
  }
  if (event.key === "Escape" && query.value) {
    event.stopPropagation();
    event.preventDefault();
    query.value = "";
  }
}

function onToggle() {
  if (!isOpen.value) {
    emit("open");
  }
  toggle();
}

watch(query, async (value) => {
  if (!value.trim()) {
    activeIndex.value = currentIndex();
    return;
  }
  activeIndex.value = 0;
  await nextTick();
  listEl.value?.scrollTo({ top: 0 });
});

watch(filtered, (items) => {
  if (activeIndex.value >= items.length) {
    activeIndex.value = Math.max(0, items.length - 1);
  }
});

onUnmounted(closeContextMenu);

watch(isOpen, async (open) => {
  if (!open) {
    closeContextMenu();
    query.value = "";
    moved.value = false;
    return;
  }
  moved.value = false;
  activeIndex.value = currentIndex();
  await nextTick();
  const menu = menuEl.value;
  if (menu) {
    menu.style.minWidth = `${menu.getBoundingClientRect().width}px`;
  }
  searchInput.value?.focus();
  scrollIntoView(".database-menu-item.active");
});
</script>

<template>
  <div class="overflow-menu database-menu">
    <button
      class="database-switch"
      type="button"
      :aria-expanded="isOpen"
      aria-haspopup="menu"
      :title="current ? `Switch ${label.toLowerCase()} from ${current}` : `Choose a ${label.toLowerCase()}`"
      @click="onToggle"
    >
      <span class="database-switch-name">{{ current || `Choose ${label.toLowerCase()}` }}</span>
      <svg viewBox="0 0 16 16" aria-hidden="true">
        <path d="M4.2 6.2L8 10l3.8-3.8" />
      </svg>
    </button>
    <div
      v-if="isOpen"
      ref="menuEl"
      class="overflow-menu-dropdown database-menu-dropdown"
      role="menu"
      :aria-label="`${label} list`"
      @pointerdown="onDropdownPointerDown"
    >
      <button
        v-if="creatable"
        class="overflow-menu-item database-menu-create"
        type="button"
        role="menuitem"
        @click="create"
      >
        <svg viewBox="0 0 16 16" aria-hidden="true">
          <path d="M8 3v10M3 8h10" />
        </svg>
        New {{ label.toLowerCase() }}…
      </button>
      <label class="database-menu-search">
        <span class="sr-only">Filter {{ label.toLowerCase() }}s</span>
        <svg class="database-menu-search-icon" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M21 21l-5.197-5.197m0 0A7.5 7.5 0 105.196 5.196a7.5 7.5 0 0010.607 10.607z" />
        </svg>
        <input
          ref="searchInput"
          v-model="query"
          type="text"
          :placeholder="`Filter ${label.toLowerCase()}s`"
          autocomplete="off"
          spellcheck="false"
          @keydown="onSearchKeydown"
        />
        <button
          v-if="query"
          class="database-menu-search-clear"
          type="button"
          title="Clear search"
          @mousedown.prevent
          @click="clearQuery"
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M6 18 18 6M6 6l12 12" />
          </svg>
        </button>
      </label>
      <div ref="listEl" class="database-menu-list" @scroll="closeContextMenu">
        <p v-if="!items.length" class="muted tiny database-menu-empty">
          No {{ label.toLowerCase() }}s.
        </p>
        <p v-else-if="!filtered.length" class="muted tiny database-menu-empty">No matches.</p>
        <button
          v-for="(item, index) in filtered"
          :key="item"
          class="overflow-menu-item database-menu-item"
          :class="{
            active: item === current,
            highlighted:
              item !== current && (contextMenu ? contextMenu.name === item : index === activeIndex),
          }"
          :data-database-index="index"
          type="button"
          role="menuitem"
          @mouseenter="onHover(index)"
          @click="select(item)"
          @contextmenu="openContextMenu($event, item)"
        >
          {{ item }}
        </button>
      </div>
      <div
        v-if="contextMenu"
        class="overflow-menu-dropdown database-context-menu"
        role="menu"
        :aria-label="`${contextMenu.name} actions`"
        :style="{ left: `${contextMenu.x}px`, top: `${contextMenu.y}px` }"
        @contextmenu.prevent
      >
        <button
          class="overflow-menu-item"
          type="button"
          role="menuitem"
          @click="runAction('openTab', contextMenu.name)"
        >
          Open in new tab
        </button>
        <button
          class="overflow-menu-item"
          type="button"
          role="menuitem"
          @click="runAction('copy', contextMenu.name)"
        >
          Copy name
        </button>
        <template v-if="renamable || droppable">
          <div class="overflow-menu-divider" role="separator" />
          <button
            v-if="renamable"
            class="overflow-menu-item"
            type="button"
            role="menuitem"
            @click="runAction('rename', contextMenu.name)"
          >
            Rename…
          </button>
          <button
            v-if="droppable"
            class="overflow-menu-item danger"
            type="button"
            role="menuitem"
            :disabled="contextMenu.name === current"
            :title="
              contextMenu.name === current
                ? `Switch to another ${label.toLowerCase()} before dropping this one`
                : undefined
            "
            @click="runAction('drop', contextMenu.name)"
          >
            Drop {{ label.toLowerCase() }}…
          </button>
        </template>
      </div>
    </div>
  </div>
</template>
