<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, useId, watch } from "vue";
import * as api from "../api";
import type { AwsIdentity, S3Location } from "../types";
import AwsProfileField from "./AwsProfileField.vue";

const location = defineModel<S3Location>({ required: true });
defineProps<{ named?: boolean }>();

const id = useId();
const profiles = ref<string[]>([]);
const profilesLoaded = ref(false);
const buckets = ref<string[]>([]);
const listing = ref(false);
const bucketError = ref("");
const advanced = ref(Boolean(location.value.endpoint || location.value.encryption));

function update(patch: Partial<S3Location>) {
  location.value = { ...location.value, ...patch };
}

async function listBuckets() {
  listing.value = true;
  bucketError.value = "";
  try {
    buckets.value = await api.listS3Buckets(location.value);
    if (!buckets.value.length) {
      bucketError.value = "This profile can't see any buckets. Type the bucket name instead.";
    }
  } catch (err) {
    bucketError.value = `${err} Type the bucket name instead.`;
  } finally {
    listing.value = false;
  }
}

const identity = ref<AwsIdentity | null>(null);
const identityError = ref("");
const checkingIdentity = ref(false);
let identityRequest = 0;
let identityTimer: ReturnType<typeof setTimeout> | undefined;

const profileLabel = computed(() => {
  const name = location.value.profile.trim() || (profiles.value.includes("default") ? "default" : "");
  return name ? `the “${name}” profile` : "the default credentials";
});

/** A readable name for the user or role in an ARN, like `role Admin (fyl)`. */
function describeArn(arn: string) {
  const resource = arn.split(":").slice(5).join(":");
  const [kind, ...rest] = resource.split("/");
  if (kind === "user") {
    return `IAM user ${rest[rest.length - 1]}`;
  }
  if (kind === "assumed-role") {
    return rest[1] ? `role ${rest[0]} (${rest[1]})` : `role ${rest[0]}`;
  }
  if (kind === "root") {
    return "the account’s root user";
  }
  return resource || arn;
}

function formatAccount(account: string) {
  return /^\d{12}$/.test(account) ? account.replace(/(\d{4})(\d{4})(\d{4})/, "$1-$2-$3") : account;
}

async function checkIdentity() {
  const request = ++identityRequest;
  checkingIdentity.value = true;
  identityError.value = "";
  try {
    const found = await api.getAwsIdentity(location.value.profile, location.value.region);
    if (request === identityRequest) {
      identity.value = found;
    }
  } catch (err) {
    if (request === identityRequest) {
      identity.value = null;
      identityError.value = String(err);
    }
  } finally {
    if (request === identityRequest) {
      checkingIdentity.value = false;
    }
  }
}

watch(
  () => [location.value.profile, location.value.region, location.value.endpoint],
  () => {
    clearTimeout(identityTimer);
    identityRequest++;
    identity.value = null;
    identityError.value = "";
    checkingIdentity.value = false;
    if (!location.value.endpoint.trim()) {
      identityTimer = setTimeout(checkIdentity, 400);
    }
  },
  { immediate: true },
);

onMounted(async () => {
  profiles.value = await api.listAwsProfiles().catch(() => []);
  profilesLoaded.value = true;
});

onBeforeUnmount(() => {
  clearTimeout(identityTimer);
  identityRequest++;
});
</script>

<template>
  <div class="form-grid">
    <label v-if="named" class="modal-label span-2">
      <span class="muted tiny">Name</span>
      <input
        :value="location.name"
        type="text"
        spellcheck="false"
        autocomplete="off"
        placeholder="Production backups"
        @input="update({ name: ($event.target as HTMLInputElement).value })"
      />
    </label>
    <AwsProfileField
      :model-value="location.profile"
      :profiles="profiles"
      :loading="!profilesLoaded"
      @update:model-value="update({ profile: $event })"
    />
    <label class="modal-label">
      <span class="muted tiny">AWS region</span>
      <input
        :value="location.region"
        type="text"
        spellcheck="false"
        autocomplete="off"
        placeholder="Profile default"
        @input="update({ region: ($event.target as HTMLInputElement).value })"
      />
    </label>
    <p class="muted tiny span-2 s3-identity">
      <template v-if="location.endpoint.trim()">
        Uses {{ profileLabel }} from your AWS CLI to sign in to this endpoint.
      </template>
      <template v-else-if="identity">
        Uses {{ profileLabel }} from your AWS CLI, signed in as
        <strong :title="identity.arn">{{ describeArn(identity.arn) }}</strong> in account
        <strong>{{ formatAccount(identity.account) }}</strong>.
      </template>
      <template v-else-if="identityError">
        Uses {{ profileLabel }} from your AWS CLI, which can’t sign in: {{ identityError }}
      </template>
      <template v-else-if="checkingIdentity">
        <span class="spinner" aria-hidden="true" />
        Checking which AWS account {{ profileLabel }} signs in to…
      </template>
      <template v-else>Uses {{ profileLabel }} from your AWS CLI.</template>
    </p>
    <label class="modal-label">
      <span class="muted tiny">Bucket</span>
      <div class="file-picker">
        <input
          :value="location.bucket"
          type="text"
          spellcheck="false"
          autocomplete="off"
          placeholder="my-backups"
          :list="`${id}-buckets`"
          @input="update({ bucket: ($event.target as HTMLInputElement).value })"
        />
        <button class="ghost" type="button" :disabled="listing" title="List the buckets this profile can see" @click="listBuckets">
          <span v-if="listing" class="spinner" aria-hidden="true" />
          {{ listing ? "Listing…" : "List" }}
        </button>
      </div>
      <datalist :id="`${id}-buckets`">
        <option v-for="name in buckets" :key="name" :value="name" />
      </datalist>
    </label>
    <label class="modal-label">
      <span class="muted tiny">Folder</span>
      <input
        :value="location.prefix"
        type="text"
        spellcheck="false"
        autocomplete="off"
        placeholder="Optional, like backups/prod/"
        @input="update({ prefix: ($event.target as HTMLInputElement).value })"
      />
    </label>
    <p v-if="bucketError" class="settings-error span-2">{{ bucketError }}</p>
    <label class="checkbox-row span-2">
      <input v-model="advanced" type="checkbox" />
      <span>Endpoint and encryption</span>
    </label>
    <template v-if="advanced">
      <label class="modal-label span-2">
        <span class="muted tiny">Custom endpoint</span>
        <input
          :value="location.endpoint"
          type="text"
          spellcheck="false"
          autocomplete="off"
          placeholder="For S3-compatible storage, like https://<account>.r2.cloudflarestorage.com"
          @input="update({ endpoint: ($event.target as HTMLInputElement).value })"
        />
      </label>
      <label class="modal-label">
        <span class="muted tiny">Server-side encryption</span>
        <select
          :value="location.encryption"
          @change="update({ encryption: ($event.target as HTMLSelectElement).value as S3Location['encryption'] })"
        >
          <option value="">Bucket default</option>
          <option value="AES256">S3 managed keys (SSE-S3)</option>
          <option value="aws:kms">AWS KMS (SSE-KMS)</option>
        </select>
      </label>
      <label v-if="location.encryption === 'aws:kms'" class="modal-label">
        <span class="muted tiny">KMS key</span>
        <input
          :value="location.kmsKeyId"
          type="text"
          spellcheck="false"
          autocomplete="off"
          placeholder="Key ID, ARN, or alias. Optional"
          @input="update({ kmsKeyId: ($event.target as HTMLInputElement).value })"
        />
      </label>
    </template>
    <p class="muted tiny span-2 ssh-hint">
      Recon runs the AWS CLI with this profile, so it uses the same access keys, SSO sign-in, or role your
      terminal does. Nothing is stored in Recon but these settings.
    </p>
  </div>
</template>
