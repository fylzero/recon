<script setup lang="ts">
import { computed, ref } from "vue";
import { confirm } from "@tauri-apps/plugin-dialog";
import * as api from "../api";
import { contrastingText } from "../color";
import { rangeIds } from "../selection";
import { useApp } from "../composables/useApp";
import { useConnectionForm } from "../composables/useConnectionForm";
import { useOverflowMenu } from "../composables/useOverflowMenu";
import { useTabs } from "../composables/useTabs";
import { driverLabel, type ConnectionEntry } from "../types";
import DriverIcon from "./DriverIcon.vue";

const lastConnectionClick = ref<string | null>(null);

const props = defineProps<{
  connection: ConnectionEntry;
  groupId: string | null;
  siblingIds: string[];
  flush?: boolean;
  sortable?: boolean;
  dragging?: boolean;
}>();

const emit = defineEmits<{
  reorderStart: [event: PointerEvent, connectionId: string];
}>();

const { removeConnection, showToast } = useApp();
const { activeId, hasTab, openConnection, openConnections, closeConnections } = useTabs();
const { openEditConnection } = useConnectionForm();
const { isOpen: menuOpen, toggle: toggleMenu, close: closeMenu } = useOverflowMenu(
  () => `connection:${props.connection.id}`,
);

const colored = computed(() => Boolean(props.flush && props.connection.headerColor));
const headerStyle = computed(() => {
  if (!colored.value) {
    return undefined;
  }
  return {
    "--group-header": props.connection.headerColor,
    "--group-header-fg": contrastingText(props.connection.headerColor),
  };
});

const target = computed(() => {
  const entry = props.connection;
  if (entry.driver === "sqlite") {
    return entry.filePath.replace(/^\/Users\/[^/]+/, "~");
  }
  const address = `${entry.user}@${entry.host}:${entry.port}`;
  return entry.database ? `${address}/${entry.database}` : address;
});

function handleClick(event: MouseEvent) {
  const node = event.target;
  if (node instanceof Element && node.closest("button, input, label, .overflow-menu, .repo-drag")) {
    return;
  }
  if (event.shiftKey && lastConnectionClick.value) {
    openConnections(
      rangeIds(props.siblingIds, lastConnectionClick.value, props.connection.id),
      props.connection.id,
    );
  } else {
    openConnection(props.connection.id);
  }
  lastConnectionClick.value = props.connection.id;
}

function onEdit() {
  closeMenu();
  openEditConnection(props.connection, props.groupId);
}

async function onReveal() {
  closeMenu();
  try {
    await api.revealPath(props.connection.filePath);
  } catch (err) {
    showToast(String(err), "error");
  }
}

async function onRemove() {
  closeMenu();
  const ok = await confirm(
    `Remove “${props.connection.name}”? Its saved password is removed from the Keychain. The database itself is not touched.`,
    { title: "Remove connection", kind: "warning", okLabel: "Remove", cancelLabel: "Cancel" },
  );
  if (!ok) {
    return;
  }
  try {
    if (hasTab(props.connection.id)) {
      closeConnections([props.connection.id]);
    }
    await removeConnection(props.connection.id);
  } catch (err) {
    showToast(String(err), "error");
  }
}
</script>

<template>
  <div
    class="repo-row connection-row"
    :class="{
      active: activeId === connection.id,
      open: hasTab(connection.id),
      flush,
      sortable,
      dragging,
      colored,
    }"
    :style="headerStyle"
    :data-connection-id="connection.id"
    @click="handleClick"
  >
    <span
      class="repo-drag"
      :class="{ spacer: !sortable }"
      role="button"
      title="Drag to reorder"
      aria-label="Drag to reorder"
      :aria-hidden="!sortable"
      @click.stop
      @pointerdown.stop="sortable && emit('reorderStart', $event, connection.id)"
    >
      <svg v-if="sortable" viewBox="0 0 16 16" aria-hidden="true">
        <circle cx="5.5" cy="4" r="1.15" />
        <circle cx="10.5" cy="4" r="1.15" />
        <circle cx="5.5" cy="8" r="1.15" />
        <circle cx="10.5" cy="8" r="1.15" />
        <circle cx="5.5" cy="12" r="1.15" />
        <circle cx="10.5" cy="12" r="1.15" />
      </svg>
    </span>
    <span class="repo-identity">
      <DriverIcon :driver="connection.driver" />
      <span class="repo-name">{{ connection.name }}</span>
      <span class="repo-label driver-badge">{{ driverLabel(connection.driver) }}</span>
    </span>
    <span class="connection-target" :title="target">{{ target }}</span>
    <div class="overflow-menu repo-menu" @click.stop>
      <button
        class="ghost tiny overflow-menu-trigger"
        type="button"
        :aria-expanded="menuOpen"
        aria-haspopup="menu"
        title="Connection actions"
        @click.stop="toggleMenu"
      >
        <svg viewBox="0 0 16 16" aria-hidden="true">
          <circle cx="8" cy="3.25" r="1.25" />
          <circle cx="8" cy="8" r="1.25" />
          <circle cx="8" cy="12.75" r="1.25" />
        </svg>
      </button>
      <div v-if="menuOpen" class="overflow-menu-dropdown" role="menu">
        <button class="overflow-menu-item" type="button" role="menuitem" @click.stop="onEdit">
          Edit connection
        </button>
        <button
          v-if="connection.driver === 'sqlite'"
          class="overflow-menu-item"
          type="button"
          role="menuitem"
          @click.stop="onReveal"
        >
          Show in Finder
        </button>
        <button
          class="overflow-menu-item danger"
          type="button"
          role="menuitem"
          @click.stop="onRemove"
        >
          Remove connection
        </button>
      </div>
    </div>
  </div>
</template>
