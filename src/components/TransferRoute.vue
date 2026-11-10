<script setup lang="ts">
import type { Driver } from "../types";
import DriverIcon from "./DriverIcon.vue";

defineProps<{
  file: string;
  path: string;
  fileLabel: string;
  target: string;
  targetLabel: string;
  driver?: Driver;
}>();
</script>

<template>
  <div class="transfer-route">
    <div class="transfer-route-end" :title="path">
      <span class="transfer-route-icon">
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="1.75"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8Z" />
          <path d="M14 3v5h5" />
          <path d="M9 13h6M9 17h4" />
        </svg>
      </span>
      <span class="transfer-route-text">
        <strong>{{ file }}</strong>
        <span>{{ fileLabel }}</span>
      </span>
    </div>
    <svg
      class="transfer-route-arrow"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      stroke-width="1.75"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
    >
      <path d="M5 12h14M13 6l6 6-6 6" />
    </svg>
    <div class="transfer-route-end" :title="target">
      <span class="transfer-route-icon">
        <DriverIcon :driver="driver" />
      </span>
      <span class="transfer-route-text">
        <strong>{{ target }}</strong>
        <span>{{ targetLabel }}</span>
      </span>
    </div>
  </div>
</template>
