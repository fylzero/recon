<script setup lang="ts">
import { computed } from "vue";
import { DRIVER_MARKS } from "../driverIcons";
import type { ConnectionKind } from "../types";

const props = defineProps<{ driver?: ConnectionKind }>();
const mark = computed(() => (props.driver ? DRIVER_MARKS[props.driver] : undefined));
</script>

<template>
  <svg
    v-if="mark"
    class="driver-icon"
    :class="`driver-${driver}`"
    :viewBox="mark.viewBox"
    fill="currentColor"
    aria-hidden="true"
  >
    <path :d="mark.path" />
  </svg>
  <svg
    v-else
    class="driver-icon"
    :class="driver ? `driver-${driver}` : undefined"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    stroke-width="2"
    stroke-linecap="round"
    stroke-linejoin="round"
    aria-hidden="true"
  >
    <ellipse cx="12" cy="5" rx="8" ry="3" />
    <path d="M4 5v6c0 1.66 3.58 3 8 3s8-1.34 8-3V5" />
    <path d="M4 11v6c0 1.66 3.58 3 8 3s8-1.34 8-3v-6" />
  </svg>
</template>
