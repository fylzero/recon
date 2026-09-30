<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import type { CompiledFilter, FilterColumn } from "../filters/compile";
import {
  appendChild,
  cloneNode,
  conditions,
  emptyGroup,
  findNode,
  hasConditions,
  insertAfter,
  MAX_CONDITIONS,
  newCondition,
  removeNode,
  updateNode,
  type FilterCondition,
  type FilterGroup,
  type MatchMode,
} from "../filters/model";
import {
  PANEL_FRACTION_DEFAULT,
  panelFraction,
  previewPanelFraction,
  savePanelFraction,
} from "../filters/panelSize";
import FilterGroupEditor from "./FilterGroupEditor.vue";

const props = defineProps<{
  root: FilterGroup;
  columns: FilterColumn[];
  compiled: CompiledFilter;
  serverIssues: Map<string, string>;
  error: string;
  autoApply: boolean;
  /** Edits made since the last Apply, when filters don't apply automatically. */
  unapplied: boolean;
  suggest?: (column: string, search: string) => Promise<string[]>;
  /** The clipboard holds filters copied from Recon. */
  canPaste: boolean;
}>();

const emit = defineEmits<{
  update: [root: FilterGroup, immediate: boolean];
  apply: [];
  collapse: [];
  clear: [];
  showSql: [];
  copy: [];
  paste: [];
  checkClipboard: [];
}>();

const MIN_HEIGHT = 76;
const MIN_GRID = 160;
const TOOLBAR = 40;
const REVEAL_MARGIN = 6;
/** Matches the `new-filter-flash` animation in styles.css. */
const FLASH_MS = 1800;

const panel = ref<HTMLElement | null>(null);
const body = ref<HTMLElement | null>(null);
const containerHeight = ref(0);
const autoOpenId = ref("");
const flashId = ref("");
let flashTimer = 0;
let observer: ResizeObserver | null = null;

const columnMap = computed(() => new Map(props.columns.map((column) => [column.name, column])));
const all = computed(() => conditions(props.root));
const atLimit = computed(() => all.value.length >= MAX_CONDITIONS);
const notApplied = computed(
  () =>
    all.value.filter(
      (node) =>
        node.enabled &&
        node.column &&
        (props.compiled.issues.has(node.id) || props.serverIssues.has(node.id) || props.compiled.drafts.has(node.id)),
    ).length,
);
const status = computed(() => {
  if (!props.autoApply && props.unapplied) {
    return "Changes not applied yet";
  }
  const applied = props.compiled.appliedCount;
  const parts = [applied ? `${applied} ${applied === 1 ? "filter" : "filters"} applied` : "No filters applied"];
  if (notApplied.value) {
    parts.push(`${notApplied.value} not applied`);
  }
  return parts.join(" · ");
});

const maxHeight = computed(() => {
  const available = containerHeight.value;
  if (!available) {
    return undefined;
  }
  const byFraction = panelFraction.value * available;
  const leaveForGrid = available - TOOLBAR - Math.max(MIN_GRID, available * 0.25);
  return Math.round(Math.max(MIN_HEIGHT, Math.min(byFraction, leaveForGrid)));
});

function change(node: FilterCondition, immediate: boolean) {
  emit("update", updateNode(props.root, node.id, () => node), immediate);
}

function remove(id: string) {
  emit("update", removeNode(props.root, id), true);
}

function duplicate(id: string) {
  const node = findNode(props.root, id);
  if (!node || node.kind !== "condition" || atLimit.value) {
    return;
  }
  const copy = cloneNode(node);
  emit("update", insertAfter(props.root, id, copy), true);
  reveal(copy.id);
  void nextTick(() => focusRow(copy.id));
}

function setMatch(groupId: string, match: MatchMode) {
  emit("update", updateNode(props.root, groupId, (group) => (group.kind === "group" ? { ...group, match } : group)), true);
}

function add(groupId = props.root.id) {
  if (atLimit.value) {
    return;
  }
  const blank = newCondition();
  autoOpenId.value = blank.id;
  emit("update", appendChild(props.root, groupId, blank), true);
  reveal(blank.id);
}

function addGroup() {
  if (atLimit.value) {
    return;
  }
  const blank = newCondition();
  autoOpenId.value = blank.id;
  const group = { ...emptyGroup(props.root.match === "all" ? "any" : "all"), children: [blank] };
  emit("update", appendChild(props.root, props.root.id, group), true);
  reveal(group.id);
}

/** Scrolls a newly added filter or group into view and briefly highlights it. */
function reveal(id: string) {
  void nextTick(() => {
    const container = body.value;
    const target = container?.querySelector<HTMLElement>(`[data-filter-id="${CSS.escape(id)}"]`);
    if (!container || !target) {
      return;
    }
    const box = container.getBoundingClientRect();
    const rect = target.getBoundingClientRect();
    let top = container.scrollTop;
    if (rect.height > box.height || rect.top < box.top) {
      top += rect.top - box.top - REVEAL_MARGIN;
    } else if (rect.bottom > box.bottom) {
      top += rect.bottom - box.bottom + REVEAL_MARGIN;
    }
    const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    container.scrollTo({ top, behavior: reduceMotion ? "auto" : "smooth" });
    flashId.value = id;
    window.clearTimeout(flashTimer);
    flashTimer = window.setTimeout(() => {
      flashId.value = "";
    }, FLASH_MS);
  });
}

function focusRow(id: string) {
  const row = panel.value?.querySelector<HTMLElement>(`[data-filter-id="${CSS.escape(id)}"]`);
  const target = row?.querySelector<HTMLElement>(".filter-value input")
    ?? row?.querySelector<HTMLElement>(".filter-value select")
    ?? row?.querySelector<HTMLElement>(".filter-column");
  target?.focus();
}

/** Focuses the first row, or starts a new one when there are none. */
function focus() {
  const first = all.value[0];
  if (!first) {
    add();
    return;
  }
  focusRow(first.id);
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === "Escape" && !event.defaultPrevented) {
    event.preventDefault();
    emit("collapse");
  } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
    event.preventDefault();
    emit("apply");
  }
}

function startResize(event: PointerEvent) {
  event.preventDefault();
  const startY = event.clientY;
  const startHeight = panel.value?.getBoundingClientRect().height ?? 0;
  const available = containerHeight.value || 1;
  document.body.classList.add("resizing-rows");
  const onMove = (move: PointerEvent) => {
    previewPanelFraction((startHeight + move.clientY - startY) / available);
  };
  const onUp = () => {
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
    window.removeEventListener("pointercancel", onUp);
    document.body.classList.remove("resizing-rows");
    savePanelFraction();
  };
  window.addEventListener("pointermove", onMove);
  window.addEventListener("pointerup", onUp);
  window.addEventListener("pointercancel", onUp);
}

function resetSize() {
  savePanelFraction(PANEL_FRACTION_DEFAULT);
}

onMounted(() => {
  const container = panel.value?.parentElement;
  if (container) {
    observer = new ResizeObserver(() => {
      containerHeight.value = container.clientHeight;
    });
    observer.observe(container);
    containerHeight.value = container.clientHeight;
  }
});

onBeforeUnmount(() => {
  observer?.disconnect();
  window.clearTimeout(flashTimer);
});

defineExpose({ focus, add, reveal });
</script>

<template>
  <section
    ref="panel"
    class="filter-panel"
    aria-label="Filters"
    :style="maxHeight ? { maxHeight: `${maxHeight}px` } : undefined"
    @keydown="onKeydown"
  >
    <div ref="body" class="filter-panel-body">
      <FilterGroupEditor
        :group="root"
        top
        :columns="columns"
        :column-map="columnMap"
        :compiled="compiled"
        :server-issues="serverIssues"
        :auto-open-id="autoOpenId"
        :flash-id="flashId"
        :suggest="suggest"
        @change="change"
        @remove="remove"
        @duplicate="duplicate"
        @set-match="setMatch"
        @add="add"
        @enter="emit('apply')"
      />
      <p v-if="error" class="filter-error-banner" role="alert">{{ error }}</p>
    </div>
    <footer class="filter-panel-footer" @pointerenter="emit('checkClipboard')">
      <button
        class="ghost tiny"
        type="button"
        :disabled="atLimit"
        :title="atLimit ? `Filters can have at most ${MAX_CONDITIONS} conditions` : 'Add a filter'"
        @click="add()"
      >
        + Add filter
      </button>
      <button
        class="ghost tiny"
        type="button"
        :disabled="atLimit"
        title="Add a group of conditions, such as state is FL or state is GA"
        @click="addGroup"
      >
        + Add group
      </button>
      <span class="muted tiny filter-panel-status" aria-live="polite">{{ status }}</span>
      <div class="filter-panel-end">
        <button
          v-if="!autoApply"
          class="primary tiny"
          type="button"
          :disabled="!unapplied"
          title="Apply filters (⌘↵)"
          @click="emit('apply')"
        >
          Apply
        </button>
        <button
          class="ghost tiny filter-clip-button"
          type="button"
          :disabled="!hasConditions(root)"
          title="Copy these filters to the clipboard to paste into another tab or share"
          @click="emit('copy')"
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <rect x="5.5" y="5.5" width="8" height="8" rx="1.5" />
            <path d="M10.5 5.5V4A1.5 1.5 0 0 0 9 2.5H4A1.5 1.5 0 0 0 2.5 4v5A1.5 1.5 0 0 0 4 10.5h1.5" />
          </svg>
          Copy
        </button>
        <button
          class="ghost tiny filter-clip-button"
          type="button"
          :disabled="!canPaste"
          :title="
            canPaste
              ? 'Paste filters copied from another tab. They\'re added to any filters already here.'
              : 'Copy filters from a tab first, then paste them here'
          "
          @click="emit('paste')"
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M5.5 3H4.5a1 1 0 0 0-1 1v9a1 1 0 0 0 1 1h7a1 1 0 0 0 1-1V4a1 1 0 0 0-1-1h-1" />
            <rect x="5.5" y="2" width="5" height="2.5" rx="0.75" />
          </svg>
          Paste
        </button>
        <button class="ghost tiny" type="button" title="Show the SQL these filters run" @click="emit('showSql')">
          Show SQL
        </button>
        <button class="ghost tiny" type="button" :disabled="!all.length" @click="emit('clear')">Clear all</button>
        <button
          class="filter-icon-button"
          type="button"
          title="Hide filters (Esc). They stay applied."
          aria-label="Hide filters"
          @click="emit('collapse')"
        >
          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="m4.5 10 3.5-3.5 3.5 3.5" /></svg>
        </button>
      </div>
    </footer>
    <div
      class="filter-resize"
      role="separator"
      aria-orientation="horizontal"
      title="Drag to resize. Double-click to reset."
      @pointerdown="startResize"
      @dblclick="resetSize"
    />
  </section>
</template>
