<script setup lang="ts">
import { computed, nextTick, ref, watch } from "vue";
import type { GridFind } from "../composables/useGridFind";

const props = defineProps<{
  find: GridFind;
  /** Where find looks, such as "on this page", when that is less than the whole table. */
  scope?: string;
}>();

const emit = defineEmits<{
  close: [];
}>();

const input = ref<HTMLInputElement | null>(null);

const count = computed(() => props.find.matches.value.length);
const where = computed(() => props.scope ?? (props.find.partial.value ? "in loaded rows" : ""));
const status = computed(() => {
  if (!props.find.query.value) {
    return where.value ? `Searches ${where.value}` : "";
  }
  const suffix = where.value ? ` ${where.value}` : "";
  if (!count.value) {
    return `No matches${suffix}`;
  }
  const total = `${count.value.toLocaleString()}${props.find.capped.value ? "+" : ""}`;
  return `${(props.find.index.value + 1).toLocaleString()} of ${total}${suffix}`;
});

watch(
  () => props.find.focusRequest.value,
  () => {
    void nextTick(() => {
      input.value?.focus();
      input.value?.select();
    });
  },
  { immediate: true },
);

function onKeydown(event: KeyboardEvent) {
  if (event.key === "Enter") {
    event.preventDefault();
    props.find.step(event.shiftKey ? -1 : 1);
  } else if (event.key === "Escape") {
    event.preventDefault();
    event.stopPropagation();
    emit("close");
  }
}
</script>

<template>
  <div class="grid-find" role="search">
    <svg class="grid-find-icon" viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="7" cy="7" r="4.25" />
      <path d="m10.2 10.2 3.3 3.3" />
    </svg>
    <input
      ref="input"
      v-model="find.query.value"
      class="grid-find-input"
      type="search"
      placeholder="Find in results"
      aria-label="Find in results"
      spellcheck="false"
      autocomplete="off"
      autocapitalize="off"
      @keydown="onKeydown"
    />
    <span class="muted tiny grid-find-status" aria-live="polite">{{ status }}</span>
    <button
      class="ghost tiny grid-find-button"
      type="button"
      title="Previous match (⇧⌘G)"
      aria-label="Previous match"
      aria-keyshortcuts="Meta+Shift+G"
      :disabled="!count"
      @click="find.step(-1)"
    >
      <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
        <path d="m7 14 5-5 5 5" />
      </svg>
    </button>
    <button
      class="ghost tiny grid-find-button"
      type="button"
      title="Next match (⌘G)"
      aria-label="Next match"
      aria-keyshortcuts="Meta+G"
      :disabled="!count"
      @click="find.step(1)"
    >
      <svg class="button-icon" viewBox="0 0 24 24" aria-hidden="true">
        <path d="m7 10 5 5 5-5" />
      </svg>
    </button>
    <button
      class="ghost tiny grid-find-button grid-find-close"
      type="button"
      title="Close (Esc)"
      aria-label="Close find"
      @click="emit('close')"
    >
      ×
    </button>
  </div>
</template>
