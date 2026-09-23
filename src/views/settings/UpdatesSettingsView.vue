<script setup lang="ts">
import { onMounted } from "vue";
import { useApp } from "../../composables/useApp";
import { useUpdater } from "../../composables/useUpdater";

const { showToast } = useApp();
const {
  status,
  statusText,
  currentVersion,
  busy,
  ensureCurrentVersion,
  checkForUpdates,
  showPrompt,
} = useUpdater();

onMounted(() => {
  void ensureCurrentVersion();
});

async function onCheckForUpdates() {
  await checkForUpdates({ prompt: true });
  if (status.value === "available") {
    showPrompt();
  } else if (status.value === "up-to-date") {
    showToast("You're on the latest version.");
  } else if (status.value === "error") {
    showToast(statusText.value, "error");
  }
}
</script>

<template>
  <div class="settings-pane settings-form-page">
    <div class="settings-inner">
      <div class="settings-header">
        <div>
          <div class="brand">Updates</div>
          <p class="muted tiny">
            Recon checks GitHub on launch and when you ask. A newer build uses the same
            confirmation as startup, then installs in place and restarts.
          </p>
        </div>
      </div>

      <section class="settings-card">
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Current version</h3>
            <p class="muted tiny">The build running on this Mac.</p>
          </div>
          <span class="muted tiny">{{ currentVersion ? `v${currentVersion}` : "…" }}</span>
        </div>
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Check for updates</h3>
            <p class="muted tiny">Compare this build to the latest GitHub release.</p>
            <p
              v-if="statusText"
              class="muted tiny"
              :class="{ 'settings-error': status === 'error' }"
            >
              {{ statusText }}
            </p>
          </div>
          <button class="ghost" type="button" :disabled="busy" @click="onCheckForUpdates">
            {{ busy && status === "checking" ? "Checking…" : "Check for updates" }}
          </button>
        </div>
      </section>
    </div>
  </div>
</template>
