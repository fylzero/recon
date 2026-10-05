<script setup lang="ts">
import type { TabularOptions } from "../types";

defineProps<{ disabled?: boolean }>();

const options = defineModel<TabularOptions>({ required: true });

const DELIMITERS = [
  { value: ",", label: "Comma" },
  { value: "\t", label: "Tab" },
  { value: ";", label: "Semicolon" },
  { value: "|", label: "Pipe" },
];

const NULLS = [
  { value: "", label: "An empty field" },
  { value: "\\N", label: "\\N" },
  { value: "NULL", label: "NULL" },
];

function patch(next: Partial<TabularOptions>) {
  options.value = { ...options.value, ...next };
}

function selected(event: Event) {
  return (event.target as HTMLSelectElement).value;
}

function checked(event: Event) {
  return (event.target as HTMLInputElement).checked;
}
</script>

<template>
  <div class="tabular-options">
    <template v-if="options.format === 'csv'">
      <div class="tabular-option-pair">
        <label class="modal-label tiny">
          Delimiter
          <select :value="options.delimiter" :disabled="disabled" @change="patch({ delimiter: selected($event) })">
            <option v-for="item in DELIMITERS" :key="item.label" :value="item.value">{{ item.label }}</option>
          </select>
        </label>
        <label class="modal-label tiny">
          Write NULL as
          <select :value="options.nullAs" :disabled="disabled" @change="patch({ nullAs: selected($event) })">
            <option v-for="item in NULLS" :key="item.label" :value="item.value">{{ item.label }}</option>
          </select>
        </label>
      </div>
      <label class="checkbox-row">
        <input type="checkbox" :checked="options.header" :disabled="disabled" @change="patch({ header: checked($event) })" />
        Column names in the first row
      </label>
      <label class="checkbox-row">
        <input type="checkbox" :checked="options.bom" :disabled="disabled" @change="patch({ bom: checked($event) })" />
        Excel-friendly (adds a UTF-8 byte order mark)
      </label>
    </template>
    <label v-else class="modal-label tiny">
      Layout
      <select
        :value="options.jsonStyle"
        :disabled="disabled"
        @change="patch({ jsonStyle: selected($event) as TabularOptions['jsonStyle'] })"
      >
        <option value="array">One array of objects (.json)</option>
        <option value="lines">One object per line (.ndjson)</option>
      </select>
    </label>
  </div>
</template>
