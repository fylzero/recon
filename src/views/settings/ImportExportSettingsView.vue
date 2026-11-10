<script setup lang="ts">
import { useApp } from "../../composables/useApp";
import { hasMachineStats } from "../../platform";
import type { PreferencesPatch, TemperatureUnit } from "../../types";

const { multithreadedImport, machineStats, temperatureUnit, savePreferences, showToast } = useApp();

async function save(patch: PreferencesPatch) {
  try {
    await savePreferences(patch);
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
          <div class="brand">Import &amp; Export</div>
          <p class="muted tiny">How SQL imports and backup restores run.</p>
        </div>
      </div>

      <section class="settings-card">
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Multithreaded import</h3>
            <p class="muted tiny">
              Import mysqldump, pg_dump, and Recon files into MySQL and Postgres over one connection
              per processor core. Other files still run one statement at a time, in order. Either way,
              a dropped connection resumes where it left off.
            </p>
          </div>
          <label class="settings-control checkbox-row">
            <input
              type="checkbox"
              :checked="multithreadedImport"
              @change="save({ multithreadedImport: ($event.target as HTMLInputElement).checked })"
            />
            <span>{{ multithreadedImport ? "On" : "Off" }}</span>
          </label>
        </div>
        <div v-if="hasMachineStats" class="settings-row">
          <div class="settings-row-copy">
            <h3>Machine stats</h3>
            <p class="muted tiny">
              Show this machine's processor load and temperature while a multithreaded import or
              restore runs.
            </p>
          </div>
          <label class="settings-control checkbox-row">
            <input
              type="checkbox"
              :checked="machineStats"
              @change="save({ machineStats: ($event.target as HTMLInputElement).checked })"
            />
            <span>{{ machineStats ? "On" : "Off" }}</span>
          </label>
        </div>
        <div v-if="hasMachineStats" class="settings-row">
          <div class="settings-row-copy">
            <h3>Temperature unit</h3>
            <p class="muted tiny">Unit for the processor temperature in machine stats.</p>
          </div>
          <label class="settings-control">
            <span class="visually-hidden">Temperature unit</span>
            <select
              :value="temperatureUnit"
              @change="
                save({
                  temperatureUnit: ($event.target as HTMLSelectElement).value as TemperatureUnit,
                })
              "
            >
              <option value="fahrenheit">Fahrenheit (°F)</option>
              <option value="celsius">Celsius (°C)</option>
            </select>
          </label>
        </div>
      </section>
    </div>
  </div>
</template>
