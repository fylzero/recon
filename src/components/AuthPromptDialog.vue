<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import * as api from "../api";
import type { AuthPrompt } from "../types";
import Modal from "./Modal.vue";

const queue = ref<AuthPrompt[]>([]);
const answer = ref("");
const input = ref<HTMLInputElement | null>(null);

const current = computed(() => queue.value[0] ?? null);
const heading = computed(() => (current.value?.title ? `Sign in to ${current.value.title}` : "Sign in"));

let stopPrompt: UnlistenFn | null = null;
let stopClosed: UnlistenFn | null = null;

function finish(value: string | null) {
  const prompt = current.value;
  if (!prompt) {
    return;
  }
  queue.value = queue.value.slice(1);
  answer.value = "";
  void api.answerPrompt(prompt.id, value).catch(() => {
    /* the connection attempt already ended */
  });
}

/**
 * Runs before Modal's document-level handler, so Escape cancels this prompt
 * instead of closing a connection form open underneath it.
 */
function onKeydown(event: KeyboardEvent) {
  if (current.value && event.key === "Escape") {
    event.preventDefault();
    event.stopImmediatePropagation();
    finish(null);
  }
}

watch(
  () => current.value?.id,
  async (id) => {
    if (id) {
      await nextTick();
      input.value?.focus();
    }
  },
);

onMounted(() => {
  window.addEventListener("keydown", onKeydown, true);
  void listen<AuthPrompt>("auth-prompt", (event) => {
    queue.value = [...queue.value, event.payload];
  }).then((unlisten) => {
    stopPrompt = unlisten;
  });
  void listen<{ id: string }>("auth-prompt-closed", (event) => {
    if (current.value?.id === event.payload.id) {
      answer.value = "";
    }
    queue.value = queue.value.filter((prompt) => prompt.id !== event.payload.id);
  }).then((unlisten) => {
    stopClosed = unlisten;
  });
});

onUnmounted(() => {
  window.removeEventListener("keydown", onKeydown, true);
  stopPrompt?.();
  stopClosed?.();
});
</script>

<template>
  <Modal v-if="current" :key="current.id" :title="heading" @close="finish(null)">
    <form class="auth-prompt" @submit.prevent="finish(answer)">
      <p v-if="current.instructions" class="auth-prompt-instructions">{{ current.instructions }}</p>
      <label class="modal-label">
        <span class="muted tiny">{{ current.prompt || "Response" }}</span>
        <input
          ref="input"
          v-model="answer"
          :type="current.secret ? 'password' : 'text'"
          autocomplete="one-time-code"
          spellcheck="false"
        />
      </label>
      <button type="submit" hidden />
    </form>
    <template #actions>
      <button class="ghost" type="button" @click="finish(null)">Cancel</button>
      <button class="primary" type="button" @click="finish(answer)">Continue</button>
    </template>
  </Modal>
</template>
