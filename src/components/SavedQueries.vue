<script setup lang="ts">
import { confirm } from "@tauri-apps/plugin-dialog";
import { computed, nextTick, onUnmounted, ref, watch } from "vue";
import { useApp } from "../composables/useApp";
import type { SavedQuery } from "../types";

const props = defineProps<{
  queries: SavedQuery[];
  selectedId: string;
  openIds: Set<string>;
}>();

const emit = defineEmits<{
  "update:selectedId": [id: string];
  open: [query: SavedQuery];
  run: [query: SavedQuery];
}>();

const { saveQuery, deleteSavedQuery, showToast } = useApp();

const filter = ref("");
const nameInput = ref<HTMLInputElement | null>(null);
const menu = ref<{ x: number; y: number; query: SavedQuery } | null>(null);
const menuEl = ref<HTMLElement | null>(null);
const nameDraft = ref("");
const descriptionDraft = ref("");
let descriptionTimer: number | undefined;

const sorted = computed(() =>
  [...props.queries].sort((a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: "base" })),
);

const filtered = computed(() => {
  const needle = filter.value.trim().toLowerCase();
  if (!needle) {
    return sorted.value;
  }
  return sorted.value.filter((query) =>
    [query.name, query.description, query.sql].some((text) => text.toLowerCase().includes(needle)),
  );
});

const selected = computed(
  () => props.queries.find((query) => query.id === props.selectedId) ?? sorted.value[0] ?? null,
);

watch(
  () => selected.value?.id,
  () => {
    nameDraft.value = selected.value?.name ?? "";
    descriptionDraft.value = selected.value?.description ?? "";
  },
  { immediate: true },
);

watch(
  () => selected.value?.name,
  (name) => {
    if (name !== undefined && document.activeElement !== nameInput.value) {
      nameDraft.value = name;
    }
  },
);

function formatUpdated(at: number) {
  return at ? new Date(at).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }) : "";
}

function preview(query: SavedQuery) {
  return query.description || query.sql.replace(/\s+/g, " ").trim();
}

async function update(query: SavedQuery, patch: Partial<Pick<SavedQuery, "name" | "description">>) {
  try {
    await saveQuery({ ...query, ...patch });
  } catch (err) {
    showToast(String(err), "error");
  }
}

function commitName() {
  const query = selected.value;
  const name = nameDraft.value.trim();
  if (!query) {
    return;
  }
  if (!name || name === query.name) {
    nameDraft.value = query.name;
    return;
  }
  void update(query, { name });
}

function cancelName() {
  nameDraft.value = selected.value?.name ?? "";
  nameInput.value?.blur();
}

function commitDescription() {
  window.clearTimeout(descriptionTimer);
  const query = selected.value;
  if (query && descriptionDraft.value.trim() !== query.description) {
    void update(query, { description: descriptionDraft.value });
  }
}

function onDescriptionInput() {
  window.clearTimeout(descriptionTimer);
  descriptionTimer = window.setTimeout(commitDescription, 600);
}

function select(query: SavedQuery) {
  commitDescription();
  emit("update:selectedId", query.id);
}

async function remove(query: SavedQuery) {
  const ok = await confirm(`Delete the saved query “${query.name}”? This can't be undone.`, {
    title: "Delete saved query",
    kind: "warning",
    okLabel: "Delete",
    cancelLabel: "Cancel",
  });
  if (!ok) {
    return;
  }
  const index = sorted.value.findIndex((item) => item.id === query.id);
  const rest = sorted.value.filter((item) => item.id !== query.id);
  try {
    await deleteSavedQuery(query.id);
    emit("update:selectedId", rest[Math.min(index, rest.length - 1)]?.id ?? "");
    showToast(`Deleted “${query.name}”`);
  } catch (err) {
    showToast(String(err), "error");
  }
}

function onMenuKeydown(event: KeyboardEvent) {
  if (event.key === "Escape") {
    event.stopImmediatePropagation();
    event.preventDefault();
    closeMenu();
  }
}

function onMenuPointerDown(event: PointerEvent) {
  if (!(event.target instanceof Node && menuEl.value?.contains(event.target))) {
    closeMenu();
  }
}

function closeMenu() {
  menu.value = null;
  document.removeEventListener("keydown", onMenuKeydown, true);
  document.removeEventListener("pointerdown", onMenuPointerDown, true);
  window.removeEventListener("blur", closeMenu);
  window.removeEventListener("resize", closeMenu);
}

function openMenu(event: MouseEvent, query: SavedQuery) {
  event.preventDefault();
  closeMenu();
  select(query);
  menu.value = { x: event.clientX, y: event.clientY, query };
  document.addEventListener("keydown", onMenuKeydown, true);
  document.addEventListener("pointerdown", onMenuPointerDown, true);
  window.addEventListener("blur", closeMenu);
  window.addEventListener("resize", closeMenu);
  void nextTick(() => {
    const current = menu.value;
    if (!menuEl.value || !current) {
      return;
    }
    const rect = menuEl.value.getBoundingClientRect();
    menu.value = {
      ...current,
      x: Math.max(4, Math.min(current.x, window.innerWidth - rect.width - 4)),
      y: Math.max(4, Math.min(current.y, window.innerHeight - rect.height - 4)),
    };
  });
}

function deleteFromMenu() {
  const query = menu.value?.query;
  closeMenu();
  if (query) {
    void remove(query);
  }
}

onUnmounted(() => {
  commitDescription();
  closeMenu();
});
</script>

<template>
  <div class="saved-queries">
    <aside class="saved-queries-list">
      <div class="db-filter">
        <input v-model="filter" type="search" placeholder="Filter saved queries" spellcheck="false" />
      </div>
      <div class="db-table-list" role="listbox" aria-label="Saved queries">
        <p v-if="!filtered.length" class="muted tiny db-list-hint">No matches.</p>
        <button
          v-for="query in filtered"
          :key="query.id"
          class="saved-query-item"
          :class="{ active: selected?.id === query.id }"
          type="button"
          role="option"
          :aria-selected="selected?.id === query.id"
          :title="`${query.name}\nDouble-click to open`"
          @click="select(query)"
          @dblclick="emit('open', query)"
          @contextmenu="openMenu($event, query)"
        >
          <span class="saved-query-item-name">{{ query.name }}</span>
          <span class="saved-query-item-preview muted">{{ preview(query) }}</span>
        </button>
      </div>
    </aside>
    <Teleport to="body">
      <div
        v-if="menu"
        ref="menuEl"
        class="overflow-menu-dropdown table-context-menu"
        role="menu"
        :aria-label="`${menu.query.name} actions`"
        :style="{ left: `${menu.x}px`, top: `${menu.y}px` }"
        @contextmenu.prevent
      >
        <button class="overflow-menu-item danger" type="button" role="menuitem" @click="deleteFromMenu">
          Delete
        </button>
      </div>
    </Teleport>
    <section v-if="selected" class="saved-query-detail">
      <div class="pane-toolbar">
        <button
          class="primary tiny"
          type="button"
          title="Open this query in a tab and run it"
          @click="emit('run', selected)"
        >
          <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
            <path d="M8 6.5v11l9-5.5Z" stroke-linejoin="round" />
          </svg>
          Run
        </button>
        <button class="ghost tiny" type="button" title="Open this query in a tab to edit it" @click="emit('open', selected)">
          {{ openIds.has(selected.id) ? "Go to tab" : "Open" }}
        </button>
        <div class="pane-toolbar-end">
          <span v-if="selected.updatedAt" class="muted tiny">Saved {{ formatUpdated(selected.updatedAt) }}</span>
        </div>
      </div>
      <div class="saved-query-body">
        <label class="modal-label">
          <span class="muted tiny">Name</span>
          <input
            ref="nameInput"
            v-model="nameDraft"
            type="text"
            autocomplete="off"
            spellcheck="false"
            @blur="commitName"
            @keydown.enter.prevent="nameInput?.blur()"
            @keydown.esc.stop="cancelName"
          />
        </label>
        <label class="modal-label">
          <span class="muted tiny">Description</span>
          <textarea
            v-model="descriptionDraft"
            class="saved-query-description"
            placeholder="What this query does, or when to use it"
            @input="onDescriptionInput"
            @blur="commitDescription"
          />
        </label>
        <div class="modal-label saved-query-sql-label">
          <span class="muted tiny">SQL</span>
          <pre class="saved-query-sql" @dblclick="emit('open', selected)">{{ selected.sql }}</pre>
          <span class="muted tiny">Open the query to change its SQL, then press ⌘S to save it.</span>
        </div>
      </div>
    </section>
  </div>
</template>
