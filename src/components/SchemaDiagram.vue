<script lang="ts">
export type DiagramScope = "schema" | "selection";
</script>

<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import * as api from "../api";
import type { SchemaDiagram } from "../types";
import { layoutDiagram, scopeTables, type DiagramEdge, type DiagramNode } from "../diagram/graph";

const props = defineProps<{
  connectionId: string;
  namespace: string;
  selected: string[];
  scope: DiagramScope;
  active: boolean;
}>();

const emit = defineEmits<{
  open: [table: string];
  "update:scope": [scope: DiagramScope];
}>();

const PAD = 40;
const MIN_SCALE = 0.05;
const MAX_SCALE = 2;
const DRAG_THRESHOLD = 3;

const diagram = ref<SchemaDiagram | null>(null);
const loading = ref(false);
const error = ref("");
const keysOnly = ref(true);
const neighbours = ref(true);
const search = ref("");
const hovered = ref<string | null>(null);
const view = ref({ x: 0, y: 0, scale: 1 });
const viewport = ref<HTMLDivElement | null>(null);
const panning = ref(false);
const markerId = `diagram-arrow-${Math.random().toString(36).slice(2, 8)}`;
let request = 0;
let needsFit = true;
let drag: { pointerId: number; startX: number; startY: number; x: number; y: number; moved: boolean } | null = null;
let dragged = false;

async function load() {
  const id = ++request;
  loading.value = true;
  error.value = "";
  try {
    const result = await api.schemaDiagram(props.connectionId, props.namespace);
    if (id === request) {
      diagram.value = result;
      needsFit = true;
    }
  } catch (err) {
    if (id === request) {
      diagram.value = null;
      error.value = String(err);
    }
  } finally {
    if (id === request) {
      loading.value = false;
    }
  }
}

watch(
  () => [props.connectionId, props.namespace] as const,
  () => {
    diagram.value = null;
    if (props.namespace) {
      void load();
    }
  },
  { immediate: true },
);

const focus = computed(() => new Set(props.scope === "selection" ? props.selected : []));

const scoped = computed(() => {
  const current = diagram.value;
  if (!current) {
    return new Set<string>();
  }
  if (props.scope === "schema") {
    return new Set(current.tables.map((table) => table.name));
  }
  return scopeTables(current, props.namespace, props.selected, neighbours.value);
});

const layout = computed(() =>
  diagram.value
    ? layoutDiagram(diagram.value, props.namespace, scoped.value, { keysOnly: keysOnly.value, focus: focus.value })
    : null,
);

const canvasWidth = computed(() => (layout.value?.width ?? 0) + PAD * 2);
const canvasHeight = computed(() => (layout.value?.height ?? 0) + PAD * 2);

const emptyMessage = computed(() => {
  if (!diagram.value) {
    return loading.value ? "Loading schema…" : "";
  }
  if (!diagram.value.tables.length) {
    return "This schema has no tables.";
  }
  if (props.scope === "selection" && !props.selected.length) {
    return "No tables selected. Select tables in the Tables view, or right-click one and choose Show diagram.";
  }
  if (!layout.value?.nodes.length) {
    return "The selected tables aren't in this schema.";
  }
  return "";
});

const summary = computed(() => {
  const current = layout.value;
  if (!current || emptyMessage.value) {
    return "";
  }
  const tables = current.nodes.length === 1 ? "1 table" : `${current.nodes.length.toLocaleString()} tables`;
  if (!current.edges.length) {
    return `${tables} · no foreign keys`;
  }
  const links = current.edges.length === 1 ? "1 foreign key" : `${current.edges.length.toLocaleString()} foreign keys`;
  return `${tables} · ${links}`;
});

const related = computed(() => {
  const name = hovered.value;
  const set = new Set<string>();
  if (!name || !layout.value) {
    return set;
  }
  set.add(name);
  for (const edge of layout.value.edges) {
    if (edge.from === name) {
      set.add(edge.to);
    } else if (edge.to === name) {
      set.add(edge.from);
    }
  }
  return set;
});

const needle = computed(() => search.value.trim().toLowerCase());

function matches(node: DiagramNode) {
  return Boolean(needle.value) && node.name.toLowerCase().includes(needle.value);
}

function nodeClass(node: DiagramNode) {
  const lit = related.value.has(node.name);
  return {
    focus: node.focus,
    lit,
    match: matches(node),
    dim: hovered.value ? !lit : Boolean(needle.value) && !matches(node),
  };
}

function edgeLit(edge: DiagramEdge) {
  return hovered.value !== null && (edge.from === hovered.value || edge.to === hovered.value);
}

function clampScale(scale: number) {
  return Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale));
}

function fit() {
  const el = viewport.value;
  if (!el || !layout.value) {
    return;
  }
  const { clientWidth: width, clientHeight: height } = el;
  if (!width || !height) {
    return;
  }
  const scale = clampScale(Math.min(width / canvasWidth.value, height / canvasHeight.value, 1));
  view.value = {
    scale,
    x: Math.max(0, (width - canvasWidth.value * scale) / 2),
    y: Math.max(0, (height - canvasHeight.value * scale) / 2),
  };
  needsFit = false;
}

async function fitWhenVisible() {
  await nextTick();
  if (props.active && needsFit) {
    fit();
  }
}

watch(layout, () => {
  needsFit = true;
  void fitWhenVisible();
});

watch(
  () => props.active,
  (active) => {
    if (active && needsFit) {
      void fitWhenVisible();
    }
  },
);

function zoomAt(factor: number, cx: number, cy: number) {
  const current = view.value;
  const scale = clampScale(current.scale * factor);
  const ratio = scale / current.scale;
  view.value = { scale, x: cx - (cx - current.x) * ratio, y: cy - (cy - current.y) * ratio };
}

function zoomBy(factor: number) {
  const el = viewport.value;
  if (el) {
    zoomAt(factor, el.clientWidth / 2, el.clientHeight / 2);
  }
}

function actualSize() {
  zoomBy(1 / view.value.scale);
}

function onWheel(event: WheelEvent) {
  if (event.ctrlKey || event.metaKey) {
    const rect = viewport.value!.getBoundingClientRect();
    zoomAt(Math.exp(-event.deltaY * 0.01), event.clientX - rect.left, event.clientY - rect.top);
    return;
  }
  view.value = { ...view.value, x: view.value.x - event.deltaX, y: view.value.y - event.deltaY };
}

function onPointerDown(event: PointerEvent) {
  if (event.button !== 0) {
    return;
  }
  dragged = false;
  drag = {
    pointerId: event.pointerId,
    startX: event.clientX,
    startY: event.clientY,
    x: view.value.x,
    y: view.value.y,
    moved: false,
  };
}

/** Capturing only once the pointer moves keeps clicks on table names working. */
function onPointerMove(event: PointerEvent) {
  if (!drag || event.pointerId !== drag.pointerId) {
    return;
  }
  const dx = event.clientX - drag.startX;
  const dy = event.clientY - drag.startY;
  if (!drag.moved && Math.hypot(dx, dy) < DRAG_THRESHOLD) {
    return;
  }
  if (!drag.moved) {
    drag.moved = true;
    panning.value = true;
    viewport.value?.setPointerCapture(event.pointerId);
  }
  view.value = { ...view.value, x: drag.x + dx, y: drag.y + dy };
}

function onPointerUp(event: PointerEvent) {
  if (!drag || event.pointerId !== drag.pointerId) {
    return;
  }
  dragged = drag.moved;
  if (viewport.value?.hasPointerCapture(event.pointerId)) {
    viewport.value.releasePointerCapture(event.pointerId);
  }
  drag = null;
  panning.value = false;
}

function openTable(name: string) {
  if (!dragged) {
    emit("open", name);
  }
}

function revealMatch() {
  const el = viewport.value;
  const node = layout.value?.nodes.find(matches);
  if (!el || !node) {
    return;
  }
  const { scale } = view.value;
  view.value = {
    scale,
    x: el.clientWidth / 2 - (node.x + PAD) * scale,
    y: el.clientHeight / 2 - (node.y + PAD) * scale,
  };
}
</script>

<template>
  <div class="diagram-pane">
    <div class="history-toolbar diagram-toolbar">
      <div class="diagram-toolbar-group">
        <div class="segmented" role="tablist" aria-label="Diagram scope">
          <button
            type="button"
            role="tab"
            :class="{ active: scope === 'schema' }"
            :aria-selected="scope === 'schema'"
            @click="emit('update:scope', 'schema')"
          >
            Whole schema
          </button>
          <button
            type="button"
            role="tab"
            :class="{ active: scope === 'selection' }"
            :aria-selected="scope === 'selection'"
            title="The tables selected in the Tables view"
            @click="emit('update:scope', 'selection')"
          >
            Selection
            <span v-if="selected.length" class="file-count-badge group-count">{{ selected.length }}</span>
          </button>
        </div>
        <button
          v-if="scope === 'selection'"
          class="history-switch"
          :class="{ on: neighbours }"
          type="button"
          role="switch"
          :aria-checked="neighbours"
          title="Also show the tables the selection references or is referenced by"
          @click="neighbours = !neighbours"
        >
          <span class="history-switch-track" aria-hidden="true">
            <span class="history-switch-knob" />
          </span>
          Neighbours
        </button>
        <button
          class="history-switch"
          :class="{ on: keysOnly }"
          type="button"
          role="switch"
          :aria-checked="keysOnly"
          title="Show only primary key and foreign key columns"
          @click="keysOnly = !keysOnly"
        >
          <span class="history-switch-track" aria-hidden="true">
            <span class="history-switch-knob" />
          </span>
          Keys only
        </button>
      </div>
      <div class="diagram-toolbar-group">
        <span v-if="summary" class="muted tiny diagram-summary">{{ summary }}</span>
        <input
          v-model="search"
          class="diagram-search"
          type="search"
          placeholder="Find table"
          spellcheck="false"
          @keydown.enter="revealMatch"
          @keydown.esc="search = ''"
        />
        <div class="segmented diagram-zoom" aria-label="Zoom">
          <button type="button" title="Zoom out" @click="zoomBy(1 / 1.2)">−</button>
          <button type="button" title="Actual size" @click="actualSize">{{ Math.round(view.scale * 100) }}%</button>
          <button type="button" title="Zoom in" @click="zoomBy(1.2)">+</button>
        </div>
        <button class="ghost tiny" type="button" title="Fit the diagram to the window" @click="fit">Fit</button>
        <button class="ghost tiny" type="button" :disabled="loading" title="Reload the schema" @click="load">
          {{ loading ? "Loading…" : "Refresh" }}
        </button>
      </div>
    </div>
    <p v-if="error" class="settings-error history-message">{{ error }}</p>
    <div
      ref="viewport"
      class="diagram-viewport"
      :class="{ panning }"
      @pointerdown="onPointerDown"
      @pointermove="onPointerMove"
      @pointerup="onPointerUp"
      @pointercancel="onPointerUp"
      @wheel.prevent="onWheel"
    >
      <p v-if="emptyMessage" class="muted tiny diagram-empty">{{ emptyMessage }}</p>
      <div
        v-else-if="layout"
        class="diagram-canvas"
        :style="{
          width: `${canvasWidth}px`,
          height: `${canvasHeight}px`,
          transform: `translate(${view.x}px, ${view.y}px) scale(${view.scale})`,
        }"
      >
        <svg class="diagram-edges" :width="canvasWidth" :height="canvasHeight" aria-hidden="true">
          <defs>
            <marker
              :id="markerId"
              viewBox="0 0 10 10"
              refX="9"
              refY="5"
              markerWidth="7"
              markerHeight="7"
              orient="auto-start-reverse"
            >
              <path class="diagram-arrow" d="M0 0 L10 5 L0 10 z" />
            </marker>
            <marker
              :id="`${markerId}-lit`"
              viewBox="0 0 10 10"
              refX="9"
              refY="5"
              markerWidth="7"
              markerHeight="7"
              orient="auto-start-reverse"
            >
              <path class="diagram-arrow lit" d="M0 0 L10 5 L0 10 z" />
            </marker>
          </defs>
          <g :transform="`translate(${PAD} ${PAD})`">
            <path
              v-for="edge in layout.edges"
              :key="edge.id"
              class="diagram-edge"
              :class="{ lit: edgeLit(edge), dim: hovered !== null && !edgeLit(edge) }"
              :d="edge.path"
              :marker-end="`url(#${edgeLit(edge) ? `${markerId}-lit` : markerId})`"
            >
              <title>{{ edge.label }}</title>
            </path>
          </g>
        </svg>
        <div
          v-for="node in layout.nodes"
          :key="node.name"
          class="diagram-table"
          :class="nodeClass(node)"
          :style="{
            left: `${node.x - node.width / 2 + PAD}px`,
            top: `${node.y - node.height / 2 + PAD}px`,
            width: `${node.width}px`,
            height: `${node.height}px`,
          }"
          @mouseenter="hovered = node.name"
          @mouseleave="hovered = null"
        >
          <button class="diagram-table-name" type="button" :title="`Open ${node.name}`" @click="openTable(node.name)">
            {{ node.name }}
          </button>
          <div
            v-for="row in node.rows"
            :key="row.name"
            class="diagram-row"
            :title="row.references ? `References ${row.references}` : undefined"
          >
            <span class="diagram-key" :class="{ pk: row.primaryKey, fk: row.foreignKey }">
              {{ row.primaryKey ? "PK" : row.foreignKey ? "FK" : "" }}
            </span>
            <span class="diagram-column">{{ row.name }}</span>
            <span class="diagram-type">{{ row.dataType }}</span>
          </div>
          <div v-if="node.hidden" class="diagram-more">
            {{ node.hidden === 1 ? "1 more column" : `${node.hidden} more columns` }}
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
