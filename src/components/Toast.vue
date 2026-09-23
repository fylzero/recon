<script setup lang="ts">
defineProps<{
  message: string;
  kind?: "success" | "error";
}>();
const emit = defineEmits<{ dismiss: [] }>();
</script>

<template>
  <div class="toast-layer" role="status" aria-live="polite">
    <button class="toast" :class="kind ?? 'success'" type="button" @click="emit('dismiss')">
      <span class="toast-icon" aria-hidden="true">
        <svg v-if="kind === 'error'" viewBox="0 0 16 16" fill="currentColor">
          <path
            d="M8 16A8 8 0 1 1 8 0a8 8 0 0 1 0 16ZM7.25 4.75a.75.75 0 0 1 1.5 0v4.5a.75.75 0 0 1-1.5 0v-4.5Zm.75 8.25a1 1 0 1 1 0-2 1 1 0 0 1 0 2Z"
          />
        </svg>
        <svg v-else viewBox="0 0 16 16" fill="currentColor">
          <path
            d="M8 16A8 8 0 1 1 8 0a8 8 0 0 1 0 16Zm3.78-9.72a.75.75 0 0 0-1.06-1.06L6.75 9.19 5.28 7.72a.75.75 0 0 0-1.06 1.06l2 2a.75.75 0 0 0 1.06 0Z"
          />
        </svg>
      </span>
      <span class="toast-copy">{{ message }}</span>
    </button>
  </div>
</template>
