<script setup lang="ts">
import { CHANGELOG } from "../changelog";
</script>

<template>
  <div class="settings-page changelog-page">
    <div class="settings-inner">
      <div class="settings-header">
        <div>
          <div class="brand">Change Log</div>
          <p class="muted tiny">What shipped in each Recon release.</p>
        </div>
      </div>
      <div class="changelog-scroll">
        <article
          v-for="release in CHANGELOG"
          :key="release.version"
          class="changelog-release"
        >
          <header class="changelog-release-head">
            <h2 class="changelog-version">{{ release.version }}</h2>
            <p class="muted tiny">{{ release.date }}</p>
          </header>
          <ul class="changelog-notes">
            <li v-for="note in release.notes" :key="note">{{ note }}</li>
          </ul>
        </article>
      </div>
    </div>
  </div>
</template>
