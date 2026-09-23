<script setup lang="ts">
import { computed, ref } from "vue";
import { useApp } from "../composables/useApp";
import { useConnectionForm } from "../composables/useConnectionForm";
import { alphabeticalIds, useDragReorder } from "../composables/useDragReorder";
import type { ConnectionGroup } from "../types";
import ConnectionGroupCard from "../components/ConnectionGroupCard.vue";
import ConnectionRow from "../components/ConnectionRow.vue";

const DRAFT_GROUP: ConnectionGroup = {
  id: "__draft__",
  name: "",
  expanded: true,
  headerColor: "#16323c",
  connections: [],
};

const {
  groups,
  standaloneConnections,
  setAllGroupsExpanded,
  reorderGroups,
  reorderConnections,
  showToast,
} = useApp();
const { openNewConnection } = useConnectionForm();
const creating = ref(false);

const groupIds = () => groups.value.map((group) => group.id);
const standaloneIds = () => standaloneConnections.value.map((connection) => connection.id);

const groupReorder = useDragReorder({
  ids: groupIds,
  selector: "[data-group-id]",
  datasetKey: "groupId",
  bodyClass: "reordering-groups",
  commit: reorderGroups,
  isBefore(element, event) {
    const header = element.querySelector(".group-header");
    const rect = (header instanceof HTMLElement ? header : element).getBoundingClientRect();
    return event.clientY <= rect.bottom && event.clientY < rect.top + rect.height / 2;
  },
});

const standaloneReorder = useDragReorder({
  ids: standaloneIds,
  selector: "[data-connection-id]",
  datasetKey: "connectionId",
  bodyClass: "reordering-repos",
  commit: (ids) => reorderConnections(null, ids),
});

const visibleGroups = computed(() => groupReorder.ordered(groups.value));
const visibleStandalone = computed(() => standaloneReorder.ordered(standaloneConnections.value));
const visibleStandaloneIds = computed(() => visibleStandalone.value.map((item) => item.id));

const isEmpty = computed(() => !groups.value.length && !standaloneConnections.value.length);
const hasGroups = computed(() => groups.value.length > 0);
const canExpandAll = computed(() => groups.value.some((group) => !group.expanded));
const canCollapseAll = computed(() => groups.value.some((group) => group.expanded));
const canSortGroups = computed(() => groups.value.length > 1);
const canSortStandalone = computed(() => standaloneConnections.value.length > 1);

const canSortGroupsAlpha = computed(
  () => canSortGroups.value && alphabeticalIds(groups.value).join("\0") !== groupIds().join("\0"),
);
const canSortStandaloneAlpha = computed(
  () =>
    canSortStandalone.value &&
    alphabeticalIds(standaloneConnections.value).join("\0") !== standaloneIds().join("\0"),
);
const canSortAlpha = computed(() => canSortGroupsAlpha.value || canSortStandaloneAlpha.value);

async function sortAlphabetically() {
  try {
    if (canSortGroupsAlpha.value) {
      await reorderGroups(alphabeticalIds(groups.value));
    }
    if (canSortStandaloneAlpha.value) {
      await reorderConnections(null, alphabeticalIds(standaloneConnections.value));
    }
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

        <div
          v-if="standaloneConnections.length"
          class="standalone-list"
          :class="{ reordering: Boolean(standaloneReorder.draggingId.value) }"
        >
          <ConnectionRow
            v-for="connection in visibleStandalone"
            :key="connection.id"
            :connection="connection"
            :group-id="null"
            :sibling-ids="visibleStandaloneIds"
            flush
            :sortable="canSortStandalone"
            :dragging="standaloneReorder.draggingId.value === connection.id"
            @reorder-start="standaloneReorder.start"
          />
        </div>

        <div class="groups-list" :class="{ reordering: Boolean(groupReorder.draggingId.value) }">
          <ConnectionGroupCard
            v-if="creating"
            :group="DRAFT_GROUP"
            draft
            @cancel="creating = false"
            @created="creating = false"
          />
          <ConnectionGroupCard
            v-for="group in visibleGroups"
            :key="group.id"
            :group="group"
            :sortable="canSortGroups"
            :dragging="groupReorder.draggingId.value === group.id"
            @reorder-start="groupReorder.start"
          />
        </div>
      </div>
    </div>
  </div>
</template>
