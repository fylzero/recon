<script setup lang="ts">
import { computed, nextTick, ref } from "vue";
import { formatDate, type FilterColumn } from "../filters/compile";
import type { FilterCondition, FilterOperator, FilterValue, MatchMode } from "../filters/model";
import {
  convertValue,
  DEFAULT_OPERATOR,
  editorFor,
  isAllowed,
  operatorHelp,
  operatorLabel,
  operatorsFor,
} from "../filters/operators";
import { ariaModifier, shortcut } from "../platform";
import ColumnPicker from "./ColumnPicker.vue";
import FilterValueInput from "./FilterValueInput.vue";

const props = defineProps<{
  node: FilterCondition;
  columns: FilterColumn[];
  column: FilterColumn | undefined;
  index: number;
  match: MatchMode;
  issue?: string;
  draft: boolean;
  duplicate: boolean;
  autoOpen: boolean;
  flash: boolean;
  suggest?: (column: string, search: string) => Promise<string[]>;
}>();

const emit = defineEmits<{
  change: [node: FilterCondition, immediate: boolean];
  remove: [];
  duplicate: [];
  setMatch: [match: MatchMode];
  enter: [];
}>();

const picker = ref<InstanceType<typeof ColumnPicker> | null>(null);
const operatorSelect = ref<HTMLSelectElement | null>(null);
const valueInput = ref<InstanceType<typeof FilterValueInput> | null>(null);

const operators = computed(() => {
  const column = props.column;
  if (!column) {
    return [];
  }
  const list = operatorsFor(column.kind, column.nullable);
  return list.includes(props.node.operator) ? list : [...list, props.node.operator];
});
const editor = computed(() => editorFor(props.node.operator));
const help = computed(() => operatorHelp(props.node.operator));
const suggestValues = computed(() => {
  const suggest = props.suggest;
  const column = props.node.column;
  return suggest && column ? (search: string) => suggest(column, search) : undefined;
});

function focusValue() {
  void nextTick(() => {
    if (editor.value === "none") {
      operatorSelect.value?.focus();
    } else {
      valueInput.value?.focus();
    }
  });
}

function pickColumn(name: string) {
  const next = props.columns.find((column) => column.name === name);
  if (!next) {
    return;
  }
  let { operator, value } = props.node;
  if (!props.node.column || props.column?.kind !== next.kind || !isAllowed(next.kind, operator)) {
    operator = DEFAULT_OPERATOR[next.kind];
    const seed = next.kind === "date" || next.kind === "datetime" ? formatDate(new Date()) : "";
    value = convertValue({ type: "single", value: seed }, operator);
  }
  emit("change", { ...props.node, column: name, operator, value }, true);
}

function setOperator(event: Event) {
  const operator = (event.target as HTMLSelectElement).value as FilterOperator;
  emit("change", { ...props.node, operator, value: convertValue(props.node.value, operator) }, true);
  focusValue();
}

function setValue(value: FilterValue, immediate: boolean) {
  emit("change", { ...props.node, value }, immediate);
}

function toggleEnabled() {
  emit("change", { ...props.node, enabled: !props.node.enabled }, true);
}

function onKeydown(event: KeyboardEvent) {
  const meta = event.metaKey || event.ctrlKey;
  if (!meta || event.altKey) {
    return;
  }
  if (event.key.toLowerCase() === "d") {
    event.preventDefault();
    emit("duplicate");
  } else if (event.key === "Backspace") {
    const target = event.target;
    // Cmd+Backspace still deletes text inside a field that has some.
    if (target instanceof HTMLInputElement && target.value) {
      return;
    }
    event.preventDefault();
    emit("remove");
  }
}

defineExpose({
  focus: () => (props.node.column ? focusValue() : picker.value?.focus()),
  openColumns: () => picker.value?.open(),
});
</script>

<template>
  <div
    class="filter-row"
    :class="{
      draft: draft && node.column,
      invalid: Boolean(issue),
      disabled: !node.enabled,
      blank: !node.column,
      'filter-flash': flash,
    }"
    :data-filter-id="node.id"
    @keydown="onKeydown"
  >
    <div class="filter-connector">
      <span v-if="index === 0">Where</span>
      <select
        v-else-if="index === 1"
        class="filter-select filter-match"
        :value="match"
        aria-label="Match all or any conditions"
        title="Whether rows must match all conditions in this group, or any one of them"
        @change="emit('setMatch', ($event.target as HTMLSelectElement).value as MatchMode)"
      >
        <option value="all">and</option>
        <option value="any">or</option>
      </select>
      <span v-else>{{ match === "all" ? "and" : "or" }}</span>
    </div>
    <ColumnPicker
      ref="picker"
      :columns="columns"
      :model-value="node.column"
      :auto-open="autoOpen"
      :invalid="Boolean(issue) && Boolean(node.column) && !column"
      @update:model-value="pickColumn"
      @picked="focusValue"
    />
    <select
      ref="operatorSelect"
      class="filter-select filter-operator"
      :value="node.operator"
      :disabled="!column"
      :title="column ? help : 'Choose a column first'"
      aria-label="Operator"
      @change="setOperator"
    >
      <option v-if="!column" :value="node.operator">Operator</option>
      <option v-for="operator in operators" :key="operator" :value="operator">
        {{ operatorLabel(column!.kind, operator) }}
      </option>
    </select>
    <FilterValueInput
      v-if="column && editor !== 'none'"
      ref="valueInput"
      :column="column"
      :operator="node.operator"
      :value="node.value"
      :invalid="Boolean(issue)"
      :suggest="suggestValues"
      @update="setValue"
      @enter="emit('enter')"
    />
    <span v-else class="filter-value-spacer" />
    <span class="filter-status">
      <span v-if="issue" class="filter-status-issue" :title="issue" aria-label="Not applied: invalid">
        <svg viewBox="0 0 16 16" aria-hidden="true">
          <path d="M8 2.5 14 13H2L8 2.5Z" />
          <path d="M8 6.5v3M8 11.2v.1" />
        </svg>
      </span>
      <span v-else-if="draft && node.column && node.enabled" class="muted tiny" title="Finish this filter to apply it">
        Not applied
      </span>
      <span
        v-else-if="duplicate"
        class="muted tiny"
        title="Same as an earlier filter in this group, so it doesn't change the results"
      >
        Duplicate
      </span>
    </span>
    <button
      class="filter-switch"
      :class="{ on: node.enabled }"
      type="button"
      role="switch"
      :aria-checked="node.enabled"
      :aria-label="`${node.column || 'Filter'} ${node.enabled ? 'on' : 'off'}`"
      :title="node.enabled ? 'Turn this filter off without removing it' : 'Turn this filter back on'"
      @click="toggleEnabled"
    >
      <span class="filter-switch-track" aria-hidden="true"><span class="filter-switch-thumb" /></span>
      <span class="filter-switch-label">{{ node.enabled ? "On" : "Off" }}</span>
    </button>
    <button
      class="filter-icon-button filter-row-duplicate"
      type="button"
      data-tip="Duplicate filter"
      :data-shortcut="shortcut('⌘D')"
      aria-label="Duplicate filter"
      :aria-keyshortcuts="`${ariaModifier}+D`"
      @click="emit('duplicate')"
    >
      <svg viewBox="0 0 16 16" aria-hidden="true">
        <rect x="5.5" y="5.5" width="8" height="8" rx="1.5" />
        <path d="M10.5 5.5V4A1.5 1.5 0 0 0 9 2.5H4A1.5 1.5 0 0 0 2.5 4v5A1.5 1.5 0 0 0 4 10.5h1.5" />
      </svg>
    </button>
    <button
      class="filter-icon-button filter-row-remove"
      type="button"
      data-tip="Remove filter"
      :data-shortcut="shortcut('⌘⌫')"
      aria-label="Remove filter"
      :aria-keyshortcuts="`${ariaModifier}+Backspace`"
      @click="emit('remove')"
    >
      ×
    </button>
  </div>
  <p v-if="issue" class="filter-issue">{{ issue }}</p>
</template>
