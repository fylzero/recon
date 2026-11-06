<script setup lang="ts">
import { computed, ref } from "vue";

const OTHER = "\0other";

const profile = defineModel<string>({ required: true });
const props = defineProps<{ profiles: string[]; loading: boolean }>();

const typing = ref(false);
const hasDefault = computed(() => props.profiles.includes("default"));

const choice = computed({
  get: () => {
    const name = profile.value.trim();
    if (!typing.value && !name && hasDefault.value) {
      return "default";
    }
    if (typing.value || (name && !props.profiles.includes(name))) {
      return OTHER;
    }
    return name;
  },
  set: (value: string) => {
    typing.value = value === OTHER;
    profile.value = typing.value ? "" : value;
  },
});
</script>

<template>
  <label class="modal-label">
    <span class="muted tiny">AWS profile</span>
    <div v-if="loading" class="select-loading">
      <select disabled>
        <option>Loading profiles…</option>
      </select>
      <span class="spinner select-loading-spinner" aria-hidden="true" />
    </div>
    <template v-else>
      <select v-if="profiles.length" v-model="choice">
        <option v-if="!hasDefault" value="">Default</option>
        <option v-for="name in profiles" :key="name" :value="name">{{ name }}</option>
        <option :value="OTHER">Other profile…</option>
      </select>
      <input
        v-if="!profiles.length || choice === OTHER"
        v-model="profile"
        type="text"
        spellcheck="false"
        autocomplete="off"
        :placeholder="profiles.length ? 'Profile name' : 'Default'"
      />
    </template>
  </label>
</template>
