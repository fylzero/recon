<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { useApp } from "../composables/useApp";
import { useConnectionForm } from "../composables/useConnectionForm";
import { useDashboardDrag } from "../composables/useDashboardDrag";
import { alphabeticalIds } from "../dashboard";
import type { ConnectionEntry, ConnectionGroup } from "../types";
import ConnectionGroupCard from "../components/ConnectionGroupCard.vue";
import ConnectionRow from "../components/ConnectionRow.vue";

const DRAFT_GROUP: ConnectionGroup = {
  id: "__draft__",
  name: "",
  expanded: true,
  headerColor: "#16323c",
  connections: [],
};

type DashboardEntry =
  | { kind: "group"; id: string; group: ConnectionGroup }
  | { kind: "connection"; id: string; connection: ConnectionEntry };

const {
  groups,
  standaloneConnections,
  dashboardIds,
  setAllGroupsExpanded,
  reorderDashboard,
  moveConnection,
  showToast,
} = useApp();
const { openNewConnection } = useConnectionForm();
const creating = ref(false);

const groupById = computed(() => new Map(groups.value.map((group) => [group.id, group])));
const connectionById = computed(() => {
  const entries = [
    ...standaloneConnections.value,
    ...groups.value.flatMap((group) => group.connections),
  ];
  return new Map(entries.map((entry) => [entry.id, entry]));
});

const drag = useDashboardDrag({
  layout: () => ({
    root: [...dashboardIds.value],
    groups: Object.fromEntries(
      groups.value.map((group) => [group.id, group.connections.map((entry) => entry.id)]),
    ),
  }),
  itemSelector: "[data-connection-id]",
  itemKey: "connectionId",
  isCollapsed: (groupId) => !groupById.value.get(groupId)?.expanded,
  commitRoot: reorderDashboard,
  commitGroup: moveConnection,
  onError: (err) => showToast(String(err), "error"),
});

const SEARCH_MIN_CONNECTIONS = 6;

const query = ref("");
const searchInput = ref<HTMLInputElement | null>(null);
const showSearch = computed(() => connectionById.value.size >= SEARCH_MIN_CONNECTIONS);
const searchTerms = computed(() =>
  showSearch.value ? query.value.trim().toLowerCase().split(/\s+/).filter(Boolean) : [],
);

watch(showSearch, (shown) => {
  if (!shown) {
    query.value = "";
  }
});
const filtering = computed(() => searchTerms.value.length > 0);

function matchesSearch(...fields: (string | undefined)[]) {
  const haystack = fields.filter(Boolean).join(" ").toLowerCase();
  return searchTerms.value.every((term) => haystack.includes(term));
}

function connectionMatches(connection: ConnectionEntry, groupName?: string) {
  return matchesSearch(
    groupName,
    connection.name,
    connection.host,
    connection.database,
    connection.filePath,
  );
}

const entries = computed(() =>
  drag.rootIds(dashboardIds.value).flatMap((id): DashboardEntry[] => {
    const group = groupById.value.get(id);
    if (group) {
      if (filtering.value && !matchesSearch(group.name) && !groupConnections(group).length) {
        return [];
      }
      return [{ kind: "group", id, group }];
    }
    const connection = connectionById.value.get(id);
    if (!connection || (filtering.value && !connectionMatches(connection))) {
      return [];
    }
    return [{ kind: "connection", id, connection }];
  }),
);
const topLevelConnectionIds = computed(() =>
  entries.value.filter((entry) => entry.kind === "connection").map((entry) => entry.id),
);

function groupConnections(group: ConnectionGroup) {
  const ids = drag.groupItemIds(group.id);
  const connections = ids
    ? ids.flatMap((id) => {
        const entry = connectionById.value.get(id);
        return entry ? [entry] : [];
      })
    : group.connections;
  return filtering.value
    ? connections.filter((connection) => connectionMatches(connection, group.name))
    : connections;
}

const totalConnectionCount = computed(() => connectionById.value.size);
const visibleConnectionCount = computed(() =>
  entries.value.reduce(
    (count, entry) =>
      count + (entry.kind === "connection" ? 1 : groupConnections(entry.group).length),
    0,
  ),
);

function clearSearch() {
  query.value = "";
  searchInput.value?.focus();
}

function onSearchKeydown(event: KeyboardEvent) {
  if (event.key !== "Escape") {
    return;
  }
  event.stopPropagation();
  if (query.value) {
    query.value = "";
    return;
  }
  searchInput.value?.blur();
}

function typingElsewhere(target: EventTarget | null) {
  if (!(target instanceof HTMLElement) || target === searchInput.value) {
    return false;
  }
  return target.isContentEditable || target.closest("input, textarea, select") !== null;
}

function onKeydown(event: KeyboardEvent) {
  if (!(event.metaKey || event.ctrlKey) || event.key.toLowerCase() !== "f") {
    return;
  }
  // The dashboard stays mounted behind connection tabs; only respond while it's visible.
  if (!searchInput.value?.offsetParent || typingElsewhere(event.target)) {
    return;
  }
  event.preventDefault();
  searchInput.value?.focus();
  searchInput.value?.select();
}

onMounted(() => {
  window.addEventListener("keydown", onKeydown);
});

onUnmounted(() => {
  window.removeEventListener("keydown", onKeydown);
});

const isEmpty = computed(() => !dashboardIds.value.length);
const hasGroups = computed(() => groups.value.length > 0);
const canExpandAll = computed(() => groups.value.some((group) => !group.expanded));
const canCollapseAll = computed(() => groups.value.some((group) => group.expanded));
const canSortEntries = computed(() => dashboardIds.value.length > 1);
const canDragConnections = computed(() => hasGroups.value || canSortEntries.value);

const alphaIds = computed(() =>
  alphabeticalIds([...groups.value, ...standaloneConnections.value]),
);
const canSortAlpha = computed(
  () => canSortEntries.value && alphaIds.value.join("\0") !== dashboardIds.value.join("\0"),
);

async function sortAlphabetically() {
  if (!canSortAlpha.value) {
    return;
  }
  try {
    await reorderDashboard(alphaIds.value);
  } catch (err) {
    showToast(String(err), "error");
  }
}
</script>

<template>
  <div class="groups-page">
    <div class="groups-inner">
      <div class="groups-header">
        <div class="brand">
          <img class="brand-icon" src="/app-icon.png" alt="" width="72" height="72" />
          Recon
        </div>
      </div>

      <div class="groups-display">
        <div class="groups-toolbar">
          <div class="toolbar-start">
            <button class="primary" type="button" @click="openNewConnection(null)">
              New connection
            </button>
            <button class="ghost" type="button" :disabled="creating" @click="creating = true">
              New group
            </button>
          </div>
          <div class="toolbar-end">
            <span v-if="filtering" class="muted tiny groups-search-count">
              {{ visibleConnectionCount }} of {{ totalConnectionCount }}
            </span>
            <label v-if="showSearch" class="groups-search-field">
              <span class="sr-only">Filter connections</span>
              <svg class="database-menu-search-icon" viewBox="0 0 24 24" aria-hidden="true">
                <path
                  d="M21 21l-5.197-5.197m0 0A7.5 7.5 0 105.196 5.196a7.5 7.5 0 0010.607 10.607z"
                />
              </svg>
              <input
                ref="searchInput"
                v-model="query"
                type="text"
                placeholder="Filter connections"
                autocapitalize="off"
                autocorrect="off"
                autocomplete="off"
                spellcheck="false"
                @keydown="onSearchKeydown"
              />
              <button
                v-if="query"
                class="database-menu-search-clear"
                type="button"
                title="Clear search"
                @click="clearSearch"
              >
                <svg viewBox="0 0 24 24" aria-hidden="true">
                  <path d="M6 18 18 6M6 6l12 12" />
                </svg>
              </button>
            </label>
            <button
              v-if="hasGroups"
              class="ghost"
              type="button"
              :disabled="!canExpandAll"
              @click="setAllGroupsExpanded(true)"
            >
              <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
                <path d="M19.5 5.25 12 12.75 4.5 5.25m15 6L12 18.75l-7.5-7.5" />
              </svg>
              Expand
            </button>
            <button
              v-if="hasGroups"
              class="ghost"
              type="button"
              :disabled="!canCollapseAll"
              @click="setAllGroupsExpanded(false)"
            >
              <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
                <path d="m4.5 18.75 7.5-7.5 7.5 7.5m-15-6 7.5-7.5 7.5 7.5" />
              </svg>
              Collapse
            </button>
            <button class="ghost" type="button" :disabled="!canSortAlpha" @click="sortAlphabetically">
              Sort A–Z
            </button>
          </div>
        </div>

        <p v-if="isEmpty && !creating" class="muted">
          Add a MySQL, PostgreSQL, or SQLite connection. Groups keep related connections together.
        </p>
        <p v-else-if="filtering && !entries.length" class="muted tiny">
          No connections match that search.
        </p>

        <div
          class="groups-list"
          data-dashboard-root
          :class="{ reordering: Boolean(drag.draggingGroupId.value) }"
        >
          <ConnectionGroupCard
            v-if="creating"
            :group="DRAFT_GROUP"
            draft
            @cancel="creating = false"
            @created="creating = false"
          />
          <template v-for="entry in entries" :key="entry.id">
            <ConnectionGroupCard
              v-if="entry.kind === 'group'"
              :data-dashboard-id="entry.id"
              :group="entry.group"
              :sortable="canSortEntries && !filtering"
              :dragging="drag.draggingGroupId.value === entry.id"
              :connections="groupConnections(entry.group)"
              :connections-sortable="canDragConnections && !filtering"
              :force-expanded="filtering"
              :dragging-connection-id="drag.draggingItemId.value"
              :drop-target="drag.dropGroupId.value === entry.id"
              @reorder-start="(event, id) => drag.start(event, 'group', id)"
              @connection-drag-start="(event, id) => drag.start(event, 'item', id)"
            />
            <ConnectionRow
              v-else
              :data-dashboard-id="entry.id"
              :connection="entry.connection"
              :group-id="null"
              :sibling-ids="topLevelConnectionIds"
              flush
              :sortable="canDragConnections && !filtering"
              :dragging="drag.draggingItemId.value === entry.id"
              @reorder-start="(event, id) => drag.start(event, 'item', id)"
            />
          </template>
        </div>
      </div>
    </div>
  </div>
</template>
