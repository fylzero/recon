<script setup lang="ts">
import { ref } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { isSsoExpired } from "../s3";
import type { S3Location } from "../types";
import Modal from "./Modal.vue";
import S3LocationFields from "./S3LocationFields.vue";

const props = defineProps<{ location: S3Location }>();

const emit = defineEmits<{
  close: [];
}>();

const { saveS3Location, showToast } = useApp();

const draft = ref<S3Location>({ ...props.location });
const saving = ref(false);
const testing = ref(false);
const signingIn = ref(false);
const error = ref("");
const tested = ref("");

async function test() {
  testing.value = true;
  error.value = "";
  tested.value = "";
  try {
    tested.value = await api.testS3Location(draft.value);
  } catch (err) {
    error.value = String(err);
  } finally {
    testing.value = false;
  }
}

async function signIn() {
  signingIn.value = true;
  try {
    await api.signInToAws(draft.value.profile);
    await test();
  } catch (err) {
    error.value = String(err);
  } finally {
    signingIn.value = false;
  }
}

async function save() {
  saving.value = true;
  error.value = "";
  try {
    const saved = await saveS3Location(draft.value);
    showToast(`Saved ${saved.name}`);
    emit("close");
  } catch (err) {
    error.value = String(err);
  } finally {
    saving.value = false;
  }
}
</script>

<template>
  <Modal :title="location.id ? 'Edit S3 location' : 'Add S3 location'" medium @close="emit('close')">
    <S3LocationFields v-model="draft" named />
    <p v-if="tested" class="muted tiny">{{ tested }}</p>
    <p v-if="error" class="settings-error">{{ error }}</p>
    <template #actions>
      <button v-if="isSsoExpired(error)" class="ghost" type="button" :disabled="signingIn" @click="signIn">
        <span v-if="signingIn" class="spinner" aria-hidden="true" />
        {{ signingIn ? "Finish signing in in your browser…" : "Sign in to AWS" }}
      </button>
      <button class="ghost" type="button" :disabled="testing || !draft.bucket.trim()" @click="test">
        <span v-if="testing" class="spinner" aria-hidden="true" />
        {{ testing ? "Testing…" : "Test" }}
      </button>
      <button class="ghost" type="button" @click="emit('close')">Cancel</button>
      <button class="primary" type="button" :disabled="saving || !draft.bucket.trim()" @click="save">
        {{ saving ? "Saving…" : "Save" }}
      </button>
    </template>
  </Modal>
</template>
