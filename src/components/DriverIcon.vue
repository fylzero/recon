<script setup lang="ts">
import type { Driver } from "../types";

defineProps<{ driver?: Driver }>();
</script>

<template>
  <svg
    class="driver-icon"
    :class="driver ? `driver-${driver}` : undefined"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="1.75"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <template v-if="driver === 'sqlite'">
      <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8Z" />
      <path d="M14 3v5h5" />
      <ellipse cx="12" cy="12.5" rx="3.5" ry="1.4" />
      <path d="M8.5 12.5v4c0 .77 1.57 1.4 3.5 1.4s3.5-.63 3.5-1.4v-4" />
    </template>
    <template v-else>
      <ellipse cx="12" cy="5" rx="8" ry="3" />
      <path d="M4 5v6c0 1.66 3.58 3 8 3s8-1.34 8-3V5" />
      <path d="M4 11v6c0 1.66 3.58 3 8 3s8-1.34 8-3v-6" />
    </template>
  </svg>
</template>
