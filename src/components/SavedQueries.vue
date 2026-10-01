<script setup lang="ts">
import type { SQLNamespace } from "@codemirror/lang-sql";
import { confirm } from "@tauri-apps/plugin-dialog";
import { computed, nextTick, onUnmounted, ref, watch } from "vue";
import { useApp } from "../composables/useApp";
import { exportSqlFile } from "../transfer";
import type { Driver, SavedQuery } from "../types";
import SqlCode from "./SqlCode.vue";

const props = defineProps<{
  queries: SavedQuery[];
  selectedId: string;
  openIds: Set<string>;
  driver: Driver;
  schema: SQLNamespace;
}>();

const emit = defineEmits<{
  "update:selectedId": [id: string];
  open: [query: SavedQuery];
  run: [query: SavedQuery];
}>();

const LIST_WIDTH_KEY = "recon.savedQueries.listWidth";
const LIST_WIDTH_DEFAULT = 272;
const LIST_WIDTH_MIN = 160;
const LIST_WIDTH_MAX = 560;

const { saveQuery, deleteSavedQuery, showToast } = useApp();

const filter = ref("");
const nameInput = ref<HTMLInputElement | null>(null);
const menu = ref<{ x: number; y: number; query: SavedQuery } | null>(null);
const menuEl = ref<HTMLElement | null>(null);
const editing = ref<{ id: string; name: string; description: string; sql: string } | null>(null);
const saving = ref(false);
const editError = ref("");
const listWidth = ref(loadListWidth());

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

const isEditing = computed(() => Boolean(editing.value && editing.value.id === selected.value?.id));

const editDirty = computed(() => {
  const draft = editing.value;
  const query = selected.value;
  if (!draft || !query || draft.id !== query.id) {
    return false;
  }
  return (
    draft.name.trim() !== query.name || draft.description.trim() !== query.description || draft.sql !== query.sql
  );
});

watch(
  () => selected.value?.id,
  (id) => {
    if (editing.value && editing.value.id !== id) {
      stopEditing();
    }
  },
);

function clampListWidth(width: number) {
  return Math.round(Math.min(LIST_WIDTH_MAX, Math.max(LIST_WIDTH_MIN, width)));
}

function loadListWidth() {
  const stored = Number(localStorage.getItem(LIST_WIDTH_KEY));
  return stored ? clampListWidth(stored) : LIST_WIDTH_DEFAULT;
}

function startListResize(event: PointerEvent) {
  event.preventDefault();
  const startX = event.clientX;
  const startWidth = listWidth.value;
  document.body.classList.add("resizing-columns");
  const onMove = (move: PointerEvent) => {
    listWidth.value = clampListWidth(startWidth + move.clientX - startX);
  };
  const onUp = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    window.removeEventListener("pointercancel", onUp);
    document.body.classList.remove("resizing-columns");
    try {
      localStorage.setItem(LIST_WIDTH_KEY, String(listWidth.value));
    } catch {
      // The width just won't persist.
    }
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onUp);
}

function resetListWidth() {
  listWidth.value = LIST_WIDTH_DEFAULT;
  localStorage.removeItem(LIST_WIDTH_KEY);
}

function formatUpdated(at: number) {
  return at ? new Date(at).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" }) : "";
}

async function confirmDiscard() {
  if (!editDirty.value) {
    return true;
  }
  return confirm(`Discard your unsaved changes to “${selected.value?.name}”?`, {
    title: "Discard changes",
    kind: "warning",
    okLabel: "Discard",
    cancelLabel: "Keep editing",
  });
}

async function select(query: SavedQuery) {
  if (query.id === selected.value?.id) {
    return true;
  }
  if (!(await confirmDiscard())) {
    return false;
  }
  stopEditing();
  emit("update:selectedId", query.id);
  return true;
}

async function startEditing(query: SavedQuery) {
  if (!(await select(query))) {
    return;
  }
  editing.value = { id: query.id, name: query.name, description: query.description, sql: query.sql };
  editError.value = "";
  void nextTick(() => {
    nameInput.value?.focus();
    nameInput.value?.select();
  });
}

function stopEditing() {
  editing.value = null;
  editError.value = "";
}

async function cancelEditing() {
  if (await confirmDiscard()) {
    stopEditing();
  }
}

function onEditEscape(event: KeyboardEvent) {
  if (event.defaultPrevented) {
    return;
  }
  event.preventDefault();
  event.stopPropagation();
  void cancelEditing();
}

async function saveEdits() {
  const draft = editing.value;
  const query = selected.value;
  if (!draft || !query || draft.id !== query.id || saving.value) {
    return;
  }
  const name = draft.name.trim();
  if (!name) {
    editError.value = "Give the query a name.";
    nameInput.value?.focus();
    return;
  }
  if (!draft.sql.trim()) {
    editError.value = "The query needs some SQL.";
    return;
  }
  if (!editDirty.value) {
    stopEditing();
    return;
  }
  saving.value = true;
  editError.value = "";
  try {
    const saved = await saveQuery({ ...query, name, description: draft.description, sql: draft.sql });
    stopEditing();
    showToast(`Saved changes to “${saved.name}”`);
  } catch (err) {
    editError.value = String(err);
  } finally {
    saving.value = false;
  }
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

async function openMenu(event: MouseEvent, query: SavedQuery) {
  event.preventDefault();
  closeMenu();
  if (!(await select(query))) {
    return;
  }
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

function fromMenu(action: (query: SavedQuery) => unknown) {
  const query = menu.value?.query;
  closeMenu();
  if (query) {
    void action(query);
  }
}

async function exportQuery(query: SavedQuery) {
  try {
    const path = await exportSqlFile(query.name, query.sql);
    if (path) {
      showToast(`Exported “${query.name}” to ${path.split("/").pop()}`);
    }
  } catch (err) {
    showToast(String(err), "error");
  }
}

onUnmounted(() => {
  closeMenu();
});

defineExpose({ isEditing, saveEdits });
</script>

<template>
  <div class="saved-queries">
    <aside class="saved-queries-list" :style="{ width: `${listWidth}px` }">
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
          <svg class="subtab-icon" viewBox="0 0 16 16" aria-hidden="true">
            <path d="M5 4 1.5 8 5 12M11 4l3.5 4L11 12" />
          </svg>
          <span class="saved-query-item-name">{{ query.name }}</span>
        </button>
      </div>
    </aside>
    <div
      class="db-sidebar-resize"
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize saved queries list"
      title="Drag to resize, double-click to reset"
      @pointerdown="startListResize"
      @dblclick="resetListWidth"
    />
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
        <button class="overflow-menu-item" type="button" role="menuitem" @click="fromMenu(startEditing)">
          Edit
        </button>
        <button class="overflow-menu-item" type="button" role="menuitem" @click="fromMenu(exportQuery)">
          Export .sql…
        </button>
        <button class="overflow-menu-item danger" type="button" role="menuitem" @click="fromMenu(remove)">
          Delete
        </button>
      </div>
    </Teleport>
    <section v-if="selected && isEditing && editing" class="saved-query-detail">
      <form class="saved-query-edit" @submit.prevent="saveEdits" @keydown.esc="onEditEscape">
        <div class="pane-toolbar">
          <button class="primary tiny" type="submit" :disabled="saving" title="Save changes (⌘S)">
            {{ saving ? "Saving…" : "Save" }}
          </button>
          <button class="ghost tiny" type="button" :disabled="saving" @click="cancelEditing">Cancel</button>
          <div class="pane-toolbar-end">
            <span v-if="editError" class="settings-error tiny">{{ editError }}</span>
          </div>
        </div>
        <div class="saved-query-body">
          <label class="modal-label">
            <span class="muted tiny">Name</span>
            <input ref="nameInput" v-model="editing.name" type="text" autocomplete="off" spellcheck="false" />
          </label>
          <label class="modal-label">
            <span class="muted tiny">Description</span>
            <textarea
              v-model="editing.description"
              class="saved-query-description"
              placeholder="What this query does, or when to use it"
            />
          </label>
          <div class="modal-label saved-query-sql-label">
            <span class="muted tiny">SQL</span>
            <SqlCode v-model="editing.sql" class="saved-query-sql editing" :driver="driver" :schema="schema" />
          </div>
        </div>
      </form>
    </section>
    <section v-else-if="selected" class="saved-query-detail">
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
        <button class="ghost tiny" type="button" title="Open this query in a tab" @click="emit('open', selected)">
          {{ openIds.has(selected.id) ? "Go to tab" : "Open" }}
        </button>
        <div class="pane-toolbar-end">
          <span v-if="selected.updatedAt" class="muted tiny">Saved {{ formatUpdated(selected.updatedAt) }}</span>
        </div>
      </div>
      <div class="saved-query-body">
        <div class="modal-label">
          <span class="muted tiny">Name</span>
          <p class="saved-query-text">{{ selected.name }}</p>
        </div>
        <div class="modal-label">
          <span class="muted tiny">Description</span>
          <p v-if="selected.description" class="saved-query-text">{{ selected.description }}</p>
          <p v-else class="saved-query-text muted">No description</p>
        </div>
        <div class="modal-label saved-query-sql-label">
          <span class="muted tiny">SQL</span>
          <SqlCode
            :model-value="selected.sql"
            class="saved-query-sql"
            :driver="driver"
            readonly
            @dblclick="emit('open', selected)"
          />
        </div>
      </div>
    </section>
  </div>
</template>
