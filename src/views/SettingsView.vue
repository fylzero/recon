<script setup lang="ts">
import { computed, defineAsyncComponent, h } from "vue";
import {
  SETTINGS_SECTIONS,
  type SettingsSection,
  useTabs,
} from "../composables/useTabs";
import AppearanceSettingsView from "./settings/AppearanceSettingsView.vue";
import GeneralSettingsView from "./settings/GeneralSettingsView.vue";
import ImportExportSettingsView from "./settings/ImportExportSettingsView.vue";
import IntegrationsSettingsView from "./settings/IntegrationsSettingsView.vue";
import WindowSettingsView from "./settings/WindowSettingsView.vue";

const JsonSettingsView = defineAsyncComponent({
  loader: () => import("./settings/JsonSettingsView.vue"),
  errorComponent: {
    setup() {
      return () =>
        h("p", { class: "settings-error settings-pane-error" }, "Could not open the JSON editor.");
    },
  },
});

const { settingsSection, openSettings } = useTabs();

const preferenceSections: { id: SettingsSection; label: string }[] = [
  { id: "general", label: "General" },
  { id: "appearance", label: "Appearance" },
  { id: "import-export", label: "Import & Export" },
  { id: "integrations", label: "Integrations" },
  { id: "window", label: "Window" },
  { id: "json", label: "JSON" },
];

const current = computed(() =>
  SETTINGS_SECTIONS.includes(settingsSection.value) ? settingsSection.value : "general",
);
</script>

<template>
  <div class="settings-layout">
    <nav class="settings-nav" aria-label="Settings">
      <button
        v-for="item in preferenceSections"
        :key="item.id"
        class="settings-nav-item"
        type="button"
        :class="{ active: current === item.id }"
        :aria-current="current === item.id ? 'page' : undefined"
        @click="openSettings(item.id)"
      >
        <svg v-if="item.id === 'general'" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M4 6h16" />
          <path d="M4 12h10" />
          <path d="M4 18h13" />
        </svg>
        <svg v-else-if="item.id === 'appearance'" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M4 19 10 5l6 14" />
          <path d="M6.5 14h7" />
          <path d="M17 19v-6" />
          <path d="M20 19v-6" />
        </svg>
        <svg v-else-if="item.id === 'import-export'" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M7 4v16" />
          <path d="m3 16 4 4 4-4" />
          <path d="M17 20V4" />
          <path d="m13 8 4-4 4 4" />
        </svg>
        <svg v-else-if="item.id === 'integrations'" viewBox="0 0 24 24" aria-hidden="true">
          <path d="M7 18a4 4 0 0 1-.6-7.96A6 6 0 0 1 18 9a4.5 4.5 0 0 1-.5 9Z" />
        </svg>
        <svg v-else-if="item.id === 'window'" viewBox="0 0 24 24" aria-hidden="true">
          <rect x="3.5" y="5.5" width="17" height="13" rx="1.5" />
          <path d="M3.5 9h17" />
        </svg>
        <svg v-else viewBox="0 0 24 24" aria-hidden="true">
          <path d="M15 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7Z" />
          <path d="M14 2v4a2 2 0 0 0 2 2h4" />
        </svg>
        {{ item.label }}
      </button>
    </nav>
    <div class="settings-content" :class="{ fill: current === 'json' }">
      <GeneralSettingsView v-if="current === 'general'" />
      <AppearanceSettingsView v-if="current === 'appearance'" />
      <ImportExportSettingsView v-if="current === 'import-export'" />
      <IntegrationsSettingsView v-if="current === 'integrations'" />
      <WindowSettingsView v-if="current === 'window'" />
      <JsonSettingsView v-if="current === 'json'" />
    </div>
  </div>
</template>
