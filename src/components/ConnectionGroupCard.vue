<script setup lang="ts">
import { computed, nextTick, onMounted, ref, watch } from "vue";
import { confirm } from "@tauri-apps/plugin-dialog";
import { useApp } from "../composables/useApp";
import { useConnectionForm } from "../composables/useConnectionForm";
import { alphabeticalIds, useDragReorder } from "../composables/useDragReorder";
import { useOverflowMenu } from "../composables/useOverflowMenu";
import { useTabs } from "../composables/useTabs";
import { contrastingText, DEFAULT_HEADER_COLOR } from "../color";
import type { ConnectionGroup } from "../types";
import ConnectionRow from "./ConnectionRow.vue";

const props = defineProps<{
  group: ConnectionGroup;
  draft?: boolean;
  sortable?: boolean;
  dragging?: boolean;
}>();

const emit = defineEmits<{
  created: [];
  cancel: [];
  reorderStart: [event: PointerEvent, groupId: string];
}>();

const { toggleGroup, createGroup, updateGroup, deleteGroup, reorderConnections, showToast } =
  useApp();
const { hasTab, closeConnections } = useTabs();
const { openNewConnection } = useConnectionForm();
const {
  isOpen: groupMenuOpen,
  toggle: toggleGroupMenu,
  close: closeMenus,
} = useOverflowMenu(() => `group:${props.group.id}`);

const connectionIds = () => props.group.connections.map((connection) => connection.id);
const reorder = useDragReorder({
  ids: connectionIds,
  selector: "[data-connection-id]",
  datasetKey: "connectionId",
  bodyClass: "reordering-repos",
  commit: (ids) => reorderConnections(props.group.id, ids),
});
const visibleConnections = computed(() => reorder.ordered(props.group.connections));
const siblingIds = computed(() => visibleConnections.value.map((connection) => connection.id));
const canSort = computed(() => !props.draft && props.group.connections.length > 1);
const canSortAlpha = computed(
  () =>
    canSort.value &&
    alphabeticalIds(props.group.connections).join("\0") !== connectionIds().join("\0"),
);

const nameInput = ref<HTMLInputElement | null>(null);
const renaming = ref(Boolean(props.draft));
const name = ref(props.draft ? "" : props.group.name);
const headerColor = ref(props.group.headerColor || DEFAULT_HEADER_COLOR);

watch(
  () => props.group,
  (group) => {
    if (props.draft || renaming.value) {
      return;
    }
    name.value = group.name;
    headerColor.value = group.headerColor || DEFAULT_HEADER_COLOR;
  },
);

onMounted(() => {
  if (props.draft) {
    void nextTick(() => nameInput.value?.focus());
  }
});

const headerStyle = computed(() => ({
  "--group-header": headerColor.value,
  "--group-header-fg": contrastingText(headerColor.value),
}));

async function sortAlphabetically() {
  closeMenus();
  if (!canSortAlpha.value) {
    return;
  }
  try {
    await reorderConnections(props.group.id, alphabeticalIds(props.group.connections));
  } catch (err) {
    showToast(String(err), "error");
  }
}

function startRename() {
  closeMenus();
  name.value = props.group.name;
  headerColor.value = props.group.headerColor || DEFAULT_HEADER_COLOR;
  renaming.value = true;
  void nextTick(() => nameInput.value?.focus());
}

function cancelRename() {
  if (props.draft) {
    emit("cancel");
    return;
  }
  renaming.value = false;
  name.value = props.group.name;
  headerColor.value = props.group.headerColor || DEFAULT_HEADER_COLOR;
}

async function finishRename() {
  const value = name.value.trim();
  try {
    if (props.draft) {
      if (!value) {
        return;
      }
      await createGroup(value, headerColor.value);
      emit("created");
      return;
    }
    renaming.value = false;
    const nextName = value || props.group.name;
    const savedColor = props.group.headerColor || DEFAULT_HEADER_COLOR;
    if (nextName !== props.group.name || headerColor.value !== savedColor) {
      await updateGroup(props.group.id, nextName, headerColor.value);
    }
    name.value = nextName;
  } catch (err) {
    showToast(String(err), "error");
  }
}

async function confirmDelete() {
  closeMenus();
  const count = props.group.connections.length;
  const ok = await confirm(
    count
      ? `Delete group “${props.group.name}” and its ${count === 1 ? "connection" : `${count} connections`}? Saved passwords are removed from the Keychain. Databases are not touched.`
      : `Delete group “${props.group.name}”?`,
    { title: "Delete group", kind: "warning", okLabel: "Delete", cancelLabel: "Cancel" },
  );
  if (!ok) {
    return;
  }
  const open = connectionIds().filter((id) => hasTab(id));
  try {
    if (open.length) {
      closeConnections(open);
    }
    await deleteGroup(props.group.id);
  } catch (err) {
    showToast(String(err), "error");
  }
}

function addConnection() {
  closeMenus();
  openNewConnection(props.group.id);
}

function onHeaderClick(event: MouseEvent) {
  if (props.draft || renaming.value) {
    return;
  }
  const target = event.target;
  if (!(target instanceof Element)) {
    return;
  }
  if (target.closest("button, input, label, select, textarea, a, .overflow-menu, .group-drag")) {
    return;
  }
  toggleGroup(props.group.id);
}
</script>

<template>
  <section
    class="group"
    :class="{ dragging, sortable }"
    :data-group-id="draft ? undefined : group.id"
  >
    <div
      class="group-header"
      :class="{ sortable }"
      :style="headerStyle"
      @click="onHeaderClick"
    >
      <span
        v-if="sortable"
        class="group-drag"
        role="button"
        title="Drag to reorder"
        aria-label="Drag to reorder"
        @click.stop
        @pointerdown.stop="emit('reorderStart', $event, group.id)"
      >
        <svg viewBox="0 0 16 16" aria-hidden="true">
          <circle cx="5.5" cy="4" r="1.15" />
          <circle cx="10.5" cy="4" r="1.15" />
          <circle cx="5.5" cy="8" r="1.15" />
          <circle cx="10.5" cy="8" r="1.15" />
          <circle cx="5.5" cy="12" r="1.15" />
          <circle cx="10.5" cy="12" r="1.15" />
        </svg>
      </span>
      <button
        class="chevron"
        type="button"
        :class="{ open: group.expanded }"
        :aria-expanded="group.expanded"
        @click="draft ? undefined : toggleGroup(group.id)"
      >
        <svg viewBox="0 0 16 16" aria-hidden="true">
          <path d="M4 6l4 4 4-4" />
        </svg>
      </button>
      <div class="group-heading">
        <input
          v-if="renaming"
          ref="nameInput"
          v-model="name"
          type="text"
          placeholder="Group name"
          @keydown.enter="finishRename"
          @keydown.escape="cancelRename"
        />
        <span v-else class="group-title">{{ group.name }}</span>
        <span v-if="!renaming" class="group-count">{{ group.connections.length }}</span>
        <label v-if="renaming" class="color-picker">
          <span class="color-picker-label">Color</span>
          <span class="color-picker-swatch" aria-hidden="true">
            <input v-model="headerColor" type="color" />
          </span>
        </label>
        <button
          v-if="renaming"
          class="ghost tiny"
          type="button"
          :disabled="draft && !name.trim()"
          @mousedown.prevent="finishRename"
        >
          Save
        </button>
        <button v-if="renaming" class="ghost tiny" type="button" @mousedown.prevent="cancelRename">
          Cancel
        </button>
      </div>
      <div class="group-actions">
        <div v-if="!draft" class="overflow-menu group-menu">
          <button
            class="ghost tiny overflow-menu-trigger"
            type="button"
            :aria-expanded="groupMenuOpen"
            aria-haspopup="menu"
            title="Group actions"
            @click.stop="toggleGroupMenu"
          >
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <circle cx="8" cy="3.25" r="1.25" />
              <circle cx="8" cy="8" r="1.25" />
              <circle cx="8" cy="12.75" r="1.25" />
            </svg>
          </button>
          <div v-if="groupMenuOpen" class="overflow-menu-dropdown" role="menu">
            <button class="overflow-menu-item" type="button" role="menuitem" @click.stop="addConnection">
              Add connection
            </button>
            <button
              class="overflow-menu-item"
              type="button"
              role="menuitem"
              :disabled="!canSortAlpha"
              @click.stop="sortAlphabetically"
            >
              Sort A–Z
            </button>
            <button class="overflow-menu-item" type="button" role="menuitem" @click.stop="startRename">
              Edit group
            </button>
            <button
              class="overflow-menu-item danger"
              type="button"
              role="menuitem"
              @click.stop="confirmDelete"
            >
              Delete group
            </button>
          </div>
        </div>
      </div>
    </div>

    <div
      v-if="group.expanded && group.connections.length"
      class="group-body"
      :class="{ reordering: Boolean(reorder.draggingId.value) }"
    >
      <ConnectionRow
        v-for="connection in visibleConnections"
        :key="connection.id"
        :connection="connection"
        :group-id="group.id"
        :sibling-ids="siblingIds"
        :sortable="canSort"
        :dragging="reorder.draggingId.value === connection.id"
        @reorder-start="reorder.start"
      />
    </div>
    <p v-else-if="group.expanded && !draft" class="muted tiny group-empty">
      No connections yet.
      <button class="link-button" type="button" @click="addConnection">Add one</button>
    </p>
  </section>
</template>
