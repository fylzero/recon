<script setup lang="ts">
import { nextTick, ref, watch } from "vue";

export interface PaneTabInfo {
  id: string;
  title: string;
  tooltip: string;
  dirty: boolean;
  filtered?: { count: number; summary: string };
  flash: boolean;
  preview?: boolean;
}

const props = defineProps<{
  paneId: string;
  focused: boolean;
  tabs: PaneTabInfo[];
  activeTabId: string;
  renamingTabId: string;
  renameValue: string;
  draggingTabId: string;
  dropHover: boolean;
}>();

const emit = defineEmits<{
  focus: [];
  select: [tabId: string];
  close: [tabId: string];
  menu: [event: MouseEvent, tabId: string];
  renameStart: [tabId: string];
  "update:renameValue": [value: string];
  renameCommit: [];
  renameCancel: [];
  filterEnter: [event: MouseEvent, tabId: string];
  filterLeave: [];
  tabPointerDown: [event: PointerEvent, tabId: string];
}>();

const stripEl = ref<HTMLElement | null>(null);

function onWheel(event: WheelEvent) {
  const strip = event.currentTarget as HTMLElement;
  if (strip.scrollWidth <= strip.clientWidth || Math.abs(event.deltaY) <= Math.abs(event.deltaX)) {
    return;
  }
  event.preventDefault();
  const scale =
    event.deltaMode === WheelEvent.DOM_DELTA_LINE ? 16 : event.deltaMode === WheelEvent.DOM_DELTA_PAGE ? strip.clientWidth : 1;
  strip.scrollLeft += event.deltaY * scale;
}

function scrollPreviewIntoView() {
  void nextTick(() => {
    const strip = stripEl.value;
    const tab = strip?.querySelector<HTMLElement>(".subtab.drop-preview");
    if (!strip || !tab) {
      return;
    }
    const box = strip.getBoundingClientRect();
    const rect = tab.getBoundingClientRect();
    if (rect.left < box.left) {
      strip.scrollLeft -= box.left - rect.left + 8;
    } else if (rect.right > box.right) {
      strip.scrollLeft += rect.right - box.right + 8;
    }
  });
}

watch(
  () => props.tabs.findIndex((tab) => tab.preview),
  (index) => {
    if (index >= 0) {
      scrollPreviewIntoView();
    }
  },
);
</script>

<template>
  <div
    class="table-pane-frame"
    :class="{ focused, 'drop-hover': dropHover }"
    :data-pane-id="paneId"
    @pointerdown="emit('focus')"
  >
    <div v-if="tabs.length" ref="stripEl" class="subtab-bar" role="tablist" @wheel="onWheel">
      <div
        v-for="tab in tabs"
        :key="tab.id"
        class="subtab"
        :class="{
          active: activeTabId === tab.id && !tab.preview,
          dirty: tab.dirty,
          'tab-flash': tab.flash,
          dragging: draggingTabId === tab.id,
          'drop-preview': tab.preview,
        }"
        :data-tab-id="tab.preview ? undefined : tab.id"
        role="tab"
        :aria-selected="activeTabId === tab.id && !tab.preview"
        :title="tab.tooltip"
        @click="!tab.preview && emit('select', tab.id)"
        @dblclick="!tab.preview && emit('renameStart', tab.id)"
        @contextmenu="!tab.preview && emit('menu', $event, tab.id)"
        @auxclick.middle="!tab.preview && emit('close', tab.id)"
        @pointerdown.stop="!tab.preview && emit('tabPointerDown', $event, tab.id)"
      >
        <span
          v-if="tab.filtered"
          class="subtab-filter"
          :aria-label="`${tab.filtered.count} ${tab.filtered.count === 1 ? 'filter' : 'filters'} applied: ${tab.filtered.summary}`"
          @mouseenter="emit('filterEnter', $event, tab.id)"
          @mouseleave="emit('filterLeave')"
        >
          <svg class="subtab-icon" viewBox="0 0 16 16" aria-hidden="true">
            <path d="M2.5 3h11L9.2 8.2v4.3l-2.4 1.2V8.2L2.5 3Z" />
          </svg>
          <span class="subtab-filter-dot" aria-hidden="true" />
        </span>
        <svg v-else class="subtab-icon" viewBox="0 0 16 16" aria-hidden="true">
          <rect x="2" y="3" width="12" height="10" rx="1.5" />
          <path d="M2 6.5h12M6.5 6.5V13" />
        </svg>
        <input
          v-if="renamingTabId === tab.id"
          class="subtab-rename"
          type="text"
          autocomplete="off"
          spellcheck="false"
          :data-rename-tab="tab.id"
          :value="renameValue"
          :size="Math.max(renameValue.length, 4)"
          :aria-label="`Rename ${tab.title}`"
          @click.stop
          @dblclick.stop
          @pointerdown.stop
          @input="emit('update:renameValue', ($event.target as HTMLInputElement).value)"
          @keydown.enter.prevent="emit('renameCommit')"
          @keydown.esc.stop.prevent="emit('renameCancel')"
          @blur="emit('renameCommit')"
        />
        <span v-else class="subtab-title">{{ tab.title }}</span>
        <button
          class="subtab-close"
          type="button"
          :aria-label="`Close ${tab.title}`"
          :aria-hidden="tab.preview || undefined"
          :tabindex="tab.preview ? -1 : undefined"
          @click.stop="!tab.preview && emit('close', tab.id)"
          @dblclick.stop
          @pointerdown.stop
        >
          <span v-if="tab.dirty" class="dirty-dot" aria-hidden="true" />
          <span class="subtab-close-icon">×</span>
        </button>
      </div>
    </div>
    <slot />
  </div>
</template>
