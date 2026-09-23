<script setup lang="ts">
import type { TableStructure } from "../types";

defineProps<{
  structure: TableStructure;
}>();
</script>

<template>
  <div class="structure-view">
    <section class="structure-section">
      <h3 class="structure-heading">
        Columns <span class="group-count">{{ structure.columns.length }}</span>
      </h3>
      <table class="structure-table">
        <thead>
          <tr>
            <th class="structure-index">#</th>
            <th>Name</th>
            <th>Type</th>
            <th>Nullable</th>
            <th>Default</th>
            <th>Extra</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="(column, index) in structure.columns" :key="column.name">
            <td class="structure-index">{{ index + 1 }}</td>
            <td class="structure-name">
              {{ column.name }}
              <span v-if="column.primaryKey" class="structure-badge key" title="Primary key">PK</span>
            </td>
            <td class="structure-type">{{ column.dataType }}</td>
            <td>{{ column.nullable ? "YES" : "NO" }}</td>
            <td :class="{ 'cell-null': column.defaultValue === null }">
              {{ column.defaultValue === null ? "NULL" : column.defaultValue }}
            </td>
            <td class="muted">{{ column.extra }}</td>
          </tr>
        </tbody>
      </table>
    </section>
    <section class="structure-section">
      <h3 class="structure-heading">
        Indexes <span class="group-count">{{ structure.indexes.length }}</span>
      </h3>
      <p v-if="!structure.indexes.length" class="muted tiny">No indexes.</p>
      <table v-else class="structure-table">
        <thead>
          <tr>
            <th>Name</th>
            <th>Columns</th>
            <th>Kind</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="index in structure.indexes" :key="index.name">
            <td class="structure-name">{{ index.name }}</td>
            <td class="structure-type">{{ index.columns }}</td>
            <td>
              <span v-if="index.primary" class="structure-badge key">PRIMARY</span>
              <span v-else-if="index.unique" class="structure-badge">UNIQUE</span>
              <span v-else class="muted">INDEX</span>
            </td>
          </tr>
        </tbody>
      </table>
    </section>
  </div>
</template>
