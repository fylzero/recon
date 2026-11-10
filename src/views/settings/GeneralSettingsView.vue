<script setup lang="ts">
import {
  DEFAULT_MAX_AUTO_COLUMN_WIDTH,
  DEFAULT_PAGE_SIZE,
  DEFAULT_QUERY_ROW_LIMIT,
  useApp,
} from "../../composables/useApp";
import type { NotificationMode, PreferencesPatch } from "../../types";
import { NOTIFICATION_MODES } from "../../notifications";
import { shortcut, systemNotificationsTarget } from "../../platform";

const PAGE_SIZE_OPTIONS = [50, 100, 200, 300, 500];
const ROW_LIMIT_OPTIONS = [1000, 5000, 10_000, 50_000, 100_000];
const COLUMN_WIDTH_OPTIONS = [240, 320, 400, 480, 640, 800];

const {
  pageSize,
  queryRowLimit,
  maxAutoColumnWidth,
  autoApplyFilters,
  notifications,
  savePreferences,
  showToast,
} = useApp();

async function save(patch: PreferencesPatch) {
  try {
    await savePreferences(patch);
  } catch (err) {
    showToast(String(err), "error");
  }
}

function selectValue(event: Event) {
  return Number((event.target as HTMLSelectElement).value);
}
</script>

<template>
  <div class="settings-pane settings-form-page">
    <div class="settings-inner">
      <div class="settings-header">
        <div>
          <div class="brand">General</div>
          <p class="muted tiny">Preferences save as you change them.</p>
        </div>
      </div>

      <section class="settings-card">
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Rows per page</h3>
            <p class="muted tiny">
              How many rows a table tab loads at a time when you open it. Each tab can pick a
              different size, up to 500.
            </p>
          </div>
          <label class="settings-control">
            <span class="visually-hidden">Rows per page</span>
            <select :value="pageSize" @change="save({ pageSize: selectValue($event) })">
              <option v-for="size in PAGE_SIZE_OPTIONS" :key="size" :value="size">
                {{ size.toLocaleString() }}{{ size === DEFAULT_PAGE_SIZE ? " (default)" : "" }}
              </option>
              <option v-if="!PAGE_SIZE_OPTIONS.includes(pageSize)" :value="pageSize">
                {{ pageSize.toLocaleString() }}
              </option>
            </select>
          </label>
        </div>
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Query row limit</h3>
            <p class="muted tiny">
              The most rows Recon keeps from a single query. Larger results are cut off at this
              limit.
            </p>
          </div>
          <label class="settings-control">
            <span class="visually-hidden">Query row limit</span>
            <select :value="queryRowLimit" @change="save({ queryRowLimit: selectValue($event) })">
              <option v-for="limit in ROW_LIMIT_OPTIONS" :key="limit" :value="limit">
                {{ limit.toLocaleString() }}{{ limit === DEFAULT_QUERY_ROW_LIMIT ? " (default)" : "" }}
              </option>
              <option v-if="!ROW_LIMIT_OPTIONS.includes(queryRowLimit)" :value="queryRowLimit">
                {{ queryRowLimit.toLocaleString() }}
              </option>
            </select>
          </label>
        </div>
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Max column width</h3>
            <p class="muted tiny">
              Columns size themselves to fit their data when results load, up to this width. You
              can still drag a column wider.
            </p>
          </div>
          <label class="settings-control">
            <span class="visually-hidden">Max column width</span>
            <select
              :value="maxAutoColumnWidth"
              @change="save({ maxAutoColumnWidth: selectValue($event) })"
            >
              <option v-for="width in COLUMN_WIDTH_OPTIONS" :key="width" :value="width">
                {{ width }} px{{ width === DEFAULT_MAX_AUTO_COLUMN_WIDTH ? " (default)" : "" }}
              </option>
              <option v-if="!COLUMN_WIDTH_OPTIONS.includes(maxAutoColumnWidth)" :value="maxAutoColumnWidth">
                {{ maxAutoColumnWidth }} px
              </option>
            </select>
          </label>
        </div>
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Apply filters automatically</h3>
            <p class="muted tiny">
              Reload the rows as you edit table filters. Turn this off for very large tables to apply
              filters only when you press Apply or {{ shortcut("⌘↵") }}.
            </p>
          </div>
          <label class="settings-control checkbox-row">
            <input
              type="checkbox"
              :checked="autoApplyFilters"
              @change="save({ autoApplyFilters: ($event.target as HTMLInputElement).checked })"
            />
            <span>{{ autoApplyFilters ? "On" : "Off" }}</span>
          </label>
        </div>
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>System notifications</h3>
            <p class="muted tiny">
              When to also send results {{ systemNotificationsTarget }}. In background only
              notifies while Recon isn't the active app.
            </p>
          </div>
          <label class="settings-control">
            <span class="visually-hidden">System notifications</span>
            <select
              :value="notifications"
              @change="
                save({
                  notifications: ($event.target as HTMLSelectElement).value as NotificationMode,
                })
              "
            >
              <option v-for="option in NOTIFICATION_MODES" :key="option.id" :value="option.id">
                {{ option.label }}
              </option>
            </select>
          </label>
        </div>
      </section>
    </div>
  </div>
</template>
