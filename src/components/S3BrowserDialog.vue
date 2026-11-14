<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { emptyS3Location, isSsoExpired, keyName, s3Url } from "../s3";
import { formatBytes } from "../transfer";
import type { S3File, S3Listing, S3Location } from "../types";
import Modal from "./Modal.vue";
import S3LocationFields from "./S3LocationFields.vue";

const OTHER = "\0other";

const props = defineProps<{
  title: string;
  /** "open" picks an existing file, "save" names one to write. */
  mode: "open" | "save";
  /** File name endings that can be picked in open mode, like ".gz". */
  accept?: string[];
  defaultName?: string;
  confirmLabel?: string;
}>();

const emit = defineEmits<{
  choose: [location: S3Location, key: string];
  close: [];
}>();

const { s3Locations } = useApp();

const choice = ref(s3Locations.value[0]?.id ?? OTHER);
const adhoc = ref<S3Location>(emptyS3Location());
const location = computed(() =>
  choice.value === OTHER ? adhoc.value : (s3Locations.value.find((item) => item.id === choice.value) ?? adhoc.value),
);
const prefix = ref("");
const listing = ref<S3Listing | null>(null);
const loading = ref(false);
const signingIn = ref(false);
const error = ref("");
const selected = ref("");
const fileName = ref(props.defaultName ?? "");

const crumbs = computed(() => {
  const parts = prefix.value.split("/").filter(Boolean);
  return parts.map((part, index) => ({ name: part, prefix: `${parts.slice(0, index + 1).join("/")}/` }));
});
const parent = computed(() => {
  const parts = prefix.value.split("/").filter(Boolean);
  return parts.length ? parts.slice(0, -1).map((part) => `${part}/`).join("") : null;
});
const saveKey = computed(() => `${prefix.value}${fileName.value.trim().replace(/^\/+/, "")}`);
const exists = computed(
  () => props.mode === "save" && Boolean(listing.value?.files.some((file) => file.key === saveKey.value)),
);
const canChoose = computed(() => {
  if (!location.value.bucket.trim() || loading.value) {
    return false;
  }
  if (props.mode === "open") {
    return Boolean(selected.value);
  }
  return Boolean(fileName.value.trim()) && !fileName.value.trim().endsWith("/");
});

function accepts(file: S3File) {
  return !props.accept?.length || props.accept.some((ending) => file.key.toLowerCase().endsWith(ending));
}

function modified(file: S3File) {
  const date = new Date(file.modified);
  return Number.isNaN(date.getTime()) ? file.modified : date.toLocaleString();
}

async function load(next: string) {
  if (!location.value.bucket.trim()) {
    return;
  }
  loading.value = true;
  error.value = "";
  selected.value = "";
  try {
    listing.value = await api.listS3Objects(location.value, next);
    prefix.value = listing.value.prefix;
  } catch (err) {
    listing.value = null;
    error.value = String(err);
  } finally {
    loading.value = false;
  }
}

function startAtLocation() {
  listing.value = null;
  prefix.value = location.value.prefix;
  void load(location.value.prefix);
}

async function signIn() {
  signingIn.value = true;
  try {
    await api.signInToAws(location.value.profile);
    await load(prefix.value);
  } catch (err) {
    error.value = String(err);
  } finally {
    signingIn.value = false;
  }
}

function pick(file: S3File) {
  if (!accepts(file)) {
    return;
  }
  if (props.mode === "open") {
    selected.value = file.key;
  } else {
    fileName.value = keyName(file.key);
  }
}

function choose(key = props.mode === "open" ? selected.value : saveKey.value) {
  if (key && location.value.bucket.trim()) {
    emit("choose", { ...location.value }, key);
  }
}

watch(choice, () => {
  listing.value = null;
  error.value = "";
  if (choice.value !== OTHER) {
    startAtLocation();
  }
});

onMounted(() => {
  if (choice.value !== OTHER) {
    startAtLocation();
  }
});
</script>

<template>
  <Modal :title="title" medium @close="emit('close')">
    <label class="modal-label">
      <span class="muted tiny">Location</span>
      <select v-model="choice">
        <option v-for="item in s3Locations" :key="item.id" :value="item.id">{{ item.name }}</option>
        <option :value="OTHER">Other bucket…</option>
      </select>
    </label>
    <template v-if="choice === OTHER">
      <S3LocationFields v-model="adhoc" />
      <div>
        <button class="ghost" type="button" :disabled="!adhoc.bucket.trim() || loading" @click="startAtLocation">
          Browse
        </button>
      </div>
    </template>
    <div v-if="listing || loading" class="s3-browser">
      <nav class="s3-crumbs" aria-label="Folder">
        <button type="button" class="link-button" @click="load('')">{{ location.bucket }}</button>
        <template v-for="crumb in crumbs" :key="crumb.prefix">
          <span class="muted">/</span>
          <button type="button" class="link-button" @click="load(crumb.prefix)">{{ crumb.name }}</button>
        </template>
        <span v-if="loading" class="spinner" aria-hidden="true" />
      </nav>
      <div class="s3-list" role="listbox" :aria-label="s3Url(location.bucket, prefix)">
        <button v-if="parent !== null" type="button" class="s3-row" @click="load(parent)">
          <span class="s3-row-name">..</span>
        </button>
        <button
          v-for="folder in listing?.folders ?? []"
          :key="folder"
          type="button"
          class="s3-row s3-folder"
          @click="load(folder)"
        >
          <span class="s3-row-name">{{ keyName(folder) }}/</span>
        </button>
        <button
          v-for="file in listing?.files ?? []"
          :key="file.key"
          type="button"
          class="s3-row"
          role="option"
          :class="{ active: selected === file.key || (mode === 'save' && saveKey === file.key) }"
          :aria-selected="selected === file.key"
          :disabled="!accepts(file)"
          @click="pick(file)"
          @dblclick="mode === 'open' && accepts(file) && choose(file.key)"
        >
          <span class="s3-row-name">{{ keyName(file.key) }}</span>
          <span class="muted tiny">{{ formatBytes(file.size) }}</span>
          <span class="muted tiny">{{ modified(file) }}</span>
        </button>
        <p v-if="listing && !listing.folders.length && !listing.files.length" class="muted tiny s3-empty">
          This folder is empty.
        </p>
      </div>
      <p v-if="listing?.truncated" class="muted tiny">Showing the first 1,000 items in this folder.</p>
    </div>
    <label v-if="mode === 'save'" class="modal-label">
      <span class="muted tiny">File name</span>
      <input v-model="fileName" type="text" spellcheck="false" autocomplete="off" />
    </label>
    <p v-if="exists" class="muted tiny">A file with this name is already in this folder. It will be replaced.</p>
    <p v-if="error" class="settings-error">{{ error }}</p>
    <template #actions>
      <button v-if="isSsoExpired(error)" class="ghost" type="button" :disabled="signingIn" @click="signIn">
        <span v-if="signingIn" class="spinner" aria-hidden="true" />
        {{ signingIn ? "Finish signing in in your browser…" : "Sign in to AWS" }}
      </button>
      <button class="ghost" type="button" @click="emit('close')">Cancel</button>
      <button class="primary" type="button" :disabled="!canChoose" @click="choose()">
        {{ confirmLabel ?? (mode === "open" ? "Open" : "Save") }}
      </button>
    </template>
  </Modal>
</template>
