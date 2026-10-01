<script lang="ts">
export type ConnectionViewTab = "tables" | "sql" | "diagram" | "history";
</script>

<script setup lang="ts">
defineProps<{
  active: ConnectionViewTab;
  tableCount: number;
  /** Whether the table list is open, or null when the current view has no table list. */
  listOpen: boolean | null;
}>();

const emit = defineEmits<{
  select: [tab: ConnectionViewTab];
  toggleList: [];
}>();
</script>

<template>
  <div class="view-tabs connection-view-tabs">
    <button
      v-if="listOpen !== null"
      class="view-tab view-tab-toggle"
      type="button"
      :aria-pressed="listOpen"
      :title="listOpen ? 'Hide the table list (⌘B)' : 'Show the table list (⌘B)'"
      @click="emit('toggleList')"
    >
      <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
        <rect x="3" y="4.5" width="18" height="15" rx="2" />
        <path d="M9 4.5v15" />
        <path v-if="listOpen" d="M5.5 8.5h1.5M5.5 11h1.5M5.5 13.5h1.5" />
      </svg>
    </button>
    <div class="connection-view-tablist" role="tablist" aria-label="Connection views">
      <button
        class="view-tab"
        :class="{ active: active === 'tables' }"
        type="button"
        role="tab"
        :aria-selected="active === 'tables'"
        @click="emit('select', 'tables')"
      >
        <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
          <path
            d="M3.375 19.5h17.25m-17.25 0a1.125 1.125 0 0 1-1.125-1.125M3.375 19.5h7.5c.621 0 1.125-.504 1.125-1.125m-9.75 0V5.625m0 12.75v-1.5c0-.621.504-1.125 1.125-1.125m18.375 2.625V5.625m0 12.75c0 .621-.504 1.125-1.125 1.125m1.125-1.125v-1.5c0-.621-.504-1.125-1.125-1.125m0 3.75h-7.5A1.125 1.125 0 0 1 12 18.375m9.75-12.75c0-.621-.504-1.125-1.125-1.125H3.375c-.621 0-1.125.504-1.125 1.125m19.5 0v1.5c0 .621-.504 1.125-1.125 1.125M2.25 5.625v1.5c0 .621.504 1.125 1.125 1.125m0 0h17.25m-17.25 0h7.5c.621 0 1.125.504 1.125 1.125M3.375 8.25c-.621 0-1.125.504-1.125 1.125v1.5c0 .621.504 1.125 1.125 1.125m17.25-3.75h-7.5c-.621 0-1.125.504-1.125 1.125m8.625-1.125c.621 0 1.125.504 1.125 1.125v1.5c0 .621-.504 1.125-1.125 1.125m-17.25 0h7.5m-7.5 0c-.621 0-1.125.504-1.125 1.125v1.5c0 .621.504 1.125 1.125 1.125M12 10.875v-1.5m0 1.5c0 .621-.504 1.125-1.125 1.125M12 10.875c0 .621.504 1.125 1.125 1.125m-2.25 0c.621 0 1.125.504 1.125 1.125M13.125 12h7.5m-7.5 0c-.621 0-1.125.504-1.125 1.125M20.625 12c.621 0 1.125.504 1.125 1.125v1.5c0 .621-.504 1.125-1.125 1.125m-17.25 0h7.5M12 14.625v-1.5m0 1.5c0 .621-.504 1.125-1.125 1.125M12 14.625c0 .621.504 1.125 1.125 1.125m-2.25 0c.621 0 1.125.504 1.125 1.125m0 1.5v-1.5m0 0c0-.621.504-1.125 1.125-1.125m0 0h7.5"
          />
        </svg>
        Tables
        <span v-if="tableCount" class="file-count-badge">{{ tableCount.toLocaleString() }}</span>
      </button>
      <button
        class="view-tab"
        :class="{ active: active === 'sql' }"
        type="button"
        role="tab"
        :aria-selected="active === 'sql'"
        @click="emit('select', 'sql')"
      >
        <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M17.25 6.75 22.5 12l-5.25 5.25m-10.5 0L1.5 12l5.25-5.25m7.5-3-4.5 16.5" />
        </svg>
        SQL
      </button>
      <button
        class="view-tab"
        :class="{ active: active === 'diagram' }"
        type="button"
        role="tab"
        :aria-selected="active === 'diagram'"
        @click="emit('select', 'diagram')"
      >
        <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
          <rect x="2.5" y="3.5" width="7" height="6" rx="1" />
          <rect x="14.5" y="14.5" width="7" height="6" rx="1" />
          <path d="M9.5 6.5h6.5a2 2 0 0 1 2 2v6" />
        </svg>
        Diagram
      </button>
      <button
        class="view-tab"
        :class="{ active: active === 'history' }"
        type="button"
        role="tab"
        :aria-selected="active === 'history'"
        @click="emit('select', 'history')"
      >
        <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M12 8v4l3 1.5" />
          <path d="M3.05 11a9 9 0 1 0 .5-3.5" />
          <path d="M3 4.5V8h3.5" />
        </svg>
        History
      </button>
    </div>
  </div>
</template>
