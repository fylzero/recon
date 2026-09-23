<script setup lang="ts">
import { computed, onMounted, ref, shallowRef, watch } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import type { BrowseResult, SortDirection, TableStructure as Structure } from "../types";
import DataGrid from "./DataGrid.vue";
import TableStructure from "./TableStructure.vue";

const props = defineProps<{
  connectionId: string;
  namespace: string;
  table: string;
  kind: "table" | "view";
}>();

const { pageSize } = useApp();

const mode = ref<"data" | "structure">("data");
const page = ref(0);
const sortColumn = ref<string | null>(null);
const sortDir = ref<SortDirection>("asc");
const result = shallowRef<BrowseResult | null>(null);
const total = ref<number | null>(null);
const structure = shallowRef<Structure | null>(null);
const loading = ref(false);
const loadingStructure = ref(false);
const error = ref("");
const structureError = ref("");
const grid = ref<InstanceType<typeof DataGrid> | null>(null);
let requestId = 0;

const rows = computed(() => result.value?.rows ?? []);
const offset = computed(() => page.value * pageSize.value);
const pageCount = computed(() =>
  total.value === null ? null : Math.max(Math.ceil(total.value / pageSize.value), 1),
);
const hasNext = computed(() =>
  total.value === null
    ? rows.value.length === pageSize.value
    : offset.value + rows.value.length < total.value,
);
const rangeLabel = computed(() => {
  if (!result.value) {
    return "";
  }
  if (!rows.value.length) {
    return total.value ? `No rows on this page of ${total.value.toLocaleString()}` : "No rows";
  }
  const first = offset.value + 1;
  const last = offset.value + rows.value.length;
  const of = total.value === null ? "" : ` of ${total.value.toLocaleString()}`;
  return `${first.toLocaleString()}–${last.toLocaleString()}${of}`;
});

async function loadData(count = false) {
  const id = ++requestId;
  loading.value = true;
  error.value = "";
  try {
    const next = await api.browseTable(props.connectionId, {
      namespace: props.namespace,
      table: props.table,
      offset: offset.value,
      limit: pageSize.value,
      orderBy: sortColumn.value,
      orderDir: sortColumn.value ? sortDir.value : null,
      count,
    });
    if (id !== requestId) {
      return;
    }
    result.value = next;
    if (count) {
      total.value = next.total;
    }
    grid.value?.scrollToTop();
  } catch (err) {
    if (id === requestId) {
      error.value = String(err);
    }
  } finally {
    if (id === requestId) {
      loading.value = false;
    }
  }
}

async function loadStructure() {
  loadingStructure.value = true;
  structureError.value = "";
  try {
    structure.value = await api.tableStructure(props.connectionId, props.namespace, props.table);
  } catch (err) {
    structureError.value = String(err);
  } finally {
    loadingStructure.value = false;
  }
}

function refresh() {
  if (mode.value === "structure") {
    void loadStructure();
  } else {
    void loadData(true);
  }
}

function goToPage(next: number) {
  const last = pageCount.value === null ? Infinity : pageCount.value - 1;
  const clamped = Math.min(Math.max(next, 0), last);
  if (clamped === page.value) {
    return;
  }
  page.value = clamped;
  void loadData();
}

function onSort(column: string) {
  if (sortColumn.value !== column) {
    sortColumn.value = column;
    sortDir.value = "asc";
  } else if (sortDir.value === "asc") {
    sortDir.value = "desc";
  } else {
    sortColumn.value = null;
    sortDir.value = "asc";
  }
  page.value = 0;
  void loadData();
}

watch(mode, (next) => {
  if (next === "structure" && !structure.value && !loadingStructure.value) {
    void loadStructure();
  }
});

watch(pageSize, () => {
  page.value = 0;
  void loadData();
});

onMounted(() => {
  void loadData(true);
});

defineExpose({ refresh });
</script>

<template>
  <div class="table-view">
    <div class="pane-toolbar">
      <div class="segmented" role="group" aria-label="Table view">
        <button
          type="button"
          :class="{ active: mode === 'data' }"
          :aria-pressed="mode === 'data'"
          @click="mode = 'data'"
        >
          Data
        </button>
        <button
          type="button"
          :class="{ active: mode === 'structure' }"
          :aria-pressed="mode === 'structure'"
          @click="mode = 'structure'"
        >
          Structure
        </button>
      </div>
      <span class="pane-toolbar-title" :title="`${namespace}.${table}`">
        {{ table }}
        <span v-if="kind === 'view'" class="structure-badge">VIEW</span>
      </span>
      <div class="pane-toolbar-end">
        <span v-if="loading || loadingStructure" class="spinner" aria-label="Loading" />
        <template v-if="mode === 'data'">
          <span class="muted tiny pager-label">{{ rangeLabel }}</span>
          <div class="pager">
            <button
              class="ghost tiny"
              type="button"
              title="First page"
              :disabled="page === 0 || loading"
              @click="goToPage(0)"
            >
              «
            </button>
            <button
              class="ghost tiny"
              type="button"
              title="Previous page"
              :disabled="page === 0 || loading"
              @click="goToPage(page - 1)"
            >
              ‹
            </button>
            <span class="muted tiny pager-page">
              Page {{ page + 1 }}<template v-if="pageCount !== null"> of {{ pageCount.toLocaleString() }}</template>
            </span>
            <button
              class="ghost tiny"
              type="button"
              title="Next page"
              :disabled="!hasNext || loading"
              @click="goToPage(page + 1)"
            >
              ›
            </button>
            <button
              class="ghost tiny"
              type="button"
              title="Last page"
              :disabled="pageCount === null || page >= pageCount - 1 || loading"
              @click="goToPage((pageCount ?? 1) - 1)"
            >
              »
            </button>
          </div>
        </template>
        <button class="ghost tiny" type="button" title="Reload" @click="refresh">
          <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
            <path
              d="M16.023 9.348h4.992v-.001M2.985 19.644v-4.992m0 0h4.992m-4.993 0 3.181 3.183a8.25 8.25 0 0 0 13.803-3.7M4.031 9.865a8.25 8.25 0 0 1 13.803-3.7l3.181 3.182m0-4.991v4.99"
            />
          </svg>
          Reload
        </button>
      </div>
    </div>

    <div v-show="mode === 'data'" class="table-view-body">
      <p v-if="error" class="pane-error">{{ error }}</p>
      <DataGrid
        v-else-if="result"
        ref="grid"
        :columns="result.columns"
        :rows="rows"
        :row-number-offset="offset"
        sortable
        :sort-column="sortColumn"
        :sort-dir="sortDir"
        @sort="onSort"
      />
    </div>
    <div v-if="mode === 'structure'" class="table-view-body scroll">
      <p v-if="structureError" class="pane-error">{{ structureError }}</p>
      <TableStructure v-else-if="structure" :structure="structure" />
    </div>
    <div class="pane-status muted tiny">
      <template v-if="mode === 'data' && result">
        {{ result.columns.length }} columns · loaded in {{ result.durationMs }}ms
        <template v-if="sortColumn"> · sorted by {{ sortColumn }} {{ sortDir }}</template>
      </template>
      <template v-else-if="mode === 'structure' && structure">
        {{ structure.columns.length }} columns · {{ structure.indexes.length }} indexes
      </template>
    </div>
  </div>
</template>
