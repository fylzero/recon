<script setup lang="ts">
import { confirm } from "@tauri-apps/plugin-dialog";
import { ref } from "vue";
import S3LocationDialog from "../../components/S3LocationDialog.vue";
import { useApp } from "../../composables/useApp";
import { emptyS3Location, locationUrl } from "../../s3";
import type { S3Location } from "../../types";

const { s3Locations, deleteS3Location, showToast } = useApp();

const editing = ref<S3Location | null>(null);

async function remove(location: S3Location) {
  const ok = await confirm(
    `Remove “${location.name}”? Recon forgets this location. Nothing in the bucket is deleted.`,
    { title: "Remove S3 location", kind: "warning", okLabel: "Remove", cancelLabel: "Cancel" },
  );
  if (!ok) {
    return;
  }
  try {
    await deleteS3Location(location.id);
  } catch (err) {
    showToast(String(err), "error");
  }
}
</script>

<template>
  <div class="settings-pane settings-form-page">
    <div class="settings-inner">
      <div class="settings-header">
        <div>
          <div class="brand">Integrations</div>
          <p class="muted tiny">Places outside this computer that Recon can back up to and import from.</p>
        </div>
      </div>

      <section class="settings-card">
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Amazon S3</h3>
            <p class="muted tiny">
              Back up straight to a bucket, and restore or import .sql files straight from one. Files stream
              through Recon without being saved on this computer. Uses the AWS CLI and its profiles, and also
              works with S3-compatible storage.
            </p>
          </div>
          <button class="ghost" type="button" @click="editing = emptyS3Location()">Add location…</button>
        </div>
        <div v-for="location in s3Locations" :key="location.id" class="settings-row s3-location-row">
          <div class="settings-row-copy">
            <h3>{{ location.name }}</h3>
            <p class="muted tiny s3-location-url" :title="locationUrl(location)">
              {{ locationUrl(location) }}
              <template v-if="location.profile"> · {{ location.profile }}</template>
              <template v-if="location.endpoint"> · {{ location.endpoint }}</template>
            </p>
          </div>
          <div class="s3-location-actions">
            <button class="ghost tiny" type="button" @click="editing = { ...location }">Edit</button>
            <button class="ghost tiny" type="button" @click="remove(location)">Remove</button>
          </div>
        </div>
      </section>
    </div>
    <S3LocationDialog v-if="editing" :location="editing" @close="editing = null" />
  </div>
</template>
