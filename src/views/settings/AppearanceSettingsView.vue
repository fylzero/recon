<script setup lang="ts">
import { ref, watch, type Ref } from "vue";
import { useApp } from "../../composables/useApp";
import {
  CUSTOM_FONT_ID,
  DEFAULT_EDITOR_FONT_SIZE,
  DEFAULT_GRID_FONT_SIZE,
  DEFAULT_LIST_FONT_SIZE,
  FONT_OPTIONS,
  LIST_FONT_OPTIONS,
  FONT_SIZE_MAX,
  FONT_SIZE_MIN,
  formatFontSize,
  isPresetFont,
} from "../../fonts";
import type { PreferencesPatch } from "../../types";

const {
  editorFontFamily,
  editorFontSize,
  gridFontFamily,
  gridFontSize,
  listFontFamily,
  listFontSize,
  savePreferences,
  previewPreferences,
  showToast,
} = useApp();

type FontKey = "editorFontFamily" | "gridFontFamily" | "listFontFamily";

function useCustomFont(family: Ref<string>, options = FONT_OPTIONS) {
  const usingCustom = ref(!isPresetFont(family.value, options));
  const customFont = ref(usingCustom.value ? family.value : "");
  watch(family, (value) => {
    usingCustom.value = !isPresetFont(value, options);
    if (usingCustom.value) {
      customFont.value = value;
    }
  });
  return { usingCustom, customFont };
}

const editorFont = useCustomFont(editorFontFamily);
const gridFont = useCustomFont(gridFontFamily);
const listFont = useCustomFont(listFontFamily, LIST_FONT_OPTIONS);

async function save(patch: PreferencesPatch) {
  try {
    await savePreferences(patch);
  } catch (err) {
    showToast(String(err), "error");
  }
}

function onFontSelect(
  event: Event,
  font: ReturnType<typeof useCustomFont>,
  key: FontKey,
) {
  const value = (event.target as HTMLSelectElement).value;
  if (value === CUSTOM_FONT_ID) {
    font.usingCustom.value = true;
    if (font.customFont.value.trim()) {
      void save({ [key]: font.customFont.value });
    }
    return;
  }
  font.usingCustom.value = false;
  void save({ [key]: value });
}

function onCustomFont(
  event: Event,
  font: ReturnType<typeof useCustomFont>,
  key: FontKey,
) {
  const value = (event.target as HTMLInputElement).value;
  font.customFont.value = value;
  if (value.trim()) {
    void save({ [key]: value });
  }
}

function sliderValue(event: Event) {
  return Number((event.target as HTMLInputElement).value);
}
</script>

<template>
  <div class="settings-pane settings-form-page">
    <div class="settings-inner">
      <div class="settings-header">
        <div>
          <div class="brand">Appearance</div>
          <p class="muted tiny">Fonts and text sizes.</p>
        </div>
      </div>

      <section class="settings-card">
        <div class="settings-row" :class="{ 'settings-row-stacked': editorFont.usingCustom.value }">
          <div class="settings-row-copy">
            <h3>Editor font</h3>
            <p class="muted tiny">Typeface used in the SQL query editor.</p>
          </div>
          <div class="settings-control settings-font">
            <label>
              <span class="visually-hidden">Editor font</span>
              <select
                :value="editorFont.usingCustom.value ? CUSTOM_FONT_ID : editorFontFamily"
                @change="onFontSelect($event, editorFont, 'editorFontFamily')"
              >
                <option v-for="option in FONT_OPTIONS" :key="option.id" :value="option.id">
                  {{ option.label }}
                </option>
                <option :value="CUSTOM_FONT_ID">Custom…</option>
              </select>
            </label>
            <label v-if="editorFont.usingCustom.value">
              <span class="visually-hidden">Custom editor font</span>
              <input
                type="text"
                :value="editorFont.customFont.value"
                placeholder="Font family name"
                spellcheck="false"
                @change="onCustomFont($event, editorFont, 'editorFontFamily')"
              />
            </label>
          </div>
        </div>
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Editor size</h3>
            <p class="muted tiny">Size of the SQL query editor text.</p>
          </div>
          <div class="settings-control settings-slider">
            <label class="settings-slider-input">
              <span class="visually-hidden">Editor font size</span>
              <input
                type="range"
                :min="FONT_SIZE_MIN"
                :max="FONT_SIZE_MAX"
                step="0.5"
                :value="editorFontSize"
                @input="previewPreferences({ editorFontSize: sliderValue($event) })"
                @change="save({ editorFontSize: sliderValue($event) })"
              />
            </label>
            <span class="settings-slider-value">{{ formatFontSize(editorFontSize) }}</span>
            <button
              class="ghost tiny"
              type="button"
              :disabled="editorFontSize === DEFAULT_EDITOR_FONT_SIZE"
              @click="save({ editorFontSize: DEFAULT_EDITOR_FONT_SIZE })"
            >
              Reset
            </button>
          </div>
        </div>
        <div class="settings-row" :class="{ 'settings-row-stacked': gridFont.usingCustom.value }">
          <div class="settings-row-copy">
            <h3>Grid font</h3>
            <p class="muted tiny">Typeface used for table data and query results.</p>
          </div>
          <div class="settings-control settings-font">
            <label>
              <span class="visually-hidden">Grid font</span>
              <select
                :value="gridFont.usingCustom.value ? CUSTOM_FONT_ID : gridFontFamily"
                @change="onFontSelect($event, gridFont, 'gridFontFamily')"
              >
                <option v-for="option in FONT_OPTIONS" :key="option.id" :value="option.id">
                  {{ option.label }}
                </option>
                <option :value="CUSTOM_FONT_ID">Custom…</option>
              </select>
            </label>
            <label v-if="gridFont.usingCustom.value">
              <span class="visually-hidden">Custom grid font</span>
              <input
                type="text"
                :value="gridFont.customFont.value"
                placeholder="Font family name"
                spellcheck="false"
                @change="onCustomFont($event, gridFont, 'gridFontFamily')"
              />
            </label>
          </div>
        </div>
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Grid size</h3>
            <p class="muted tiny">Size of table data and query result text.</p>
          </div>
          <div class="settings-control settings-slider">
            <label class="settings-slider-input">
              <span class="visually-hidden">Grid font size</span>
              <input
                type="range"
                :min="FONT_SIZE_MIN"
                :max="FONT_SIZE_MAX"
                step="0.5"
                :value="gridFontSize"
                @input="previewPreferences({ gridFontSize: sliderValue($event) })"
                @change="save({ gridFontSize: sliderValue($event) })"
              />
            </label>
            <span class="settings-slider-value">{{ formatFontSize(gridFontSize) }}</span>
            <button
              class="ghost tiny"
              type="button"
              :disabled="gridFontSize === DEFAULT_GRID_FONT_SIZE"
              @click="save({ gridFontSize: DEFAULT_GRID_FONT_SIZE })"
            >
              Reset
            </button>
          </div>
        </div>
        <div class="settings-row" :class="{ 'settings-row-stacked': listFont.usingCustom.value }">
          <div class="settings-row-copy">
            <h3>Table list font</h3>
            <p class="muted tiny">Typeface used for the table list in the connection sidebar.</p>
          </div>
          <div class="settings-control settings-font">
            <label>
              <span class="visually-hidden">Table list font</span>
              <select
                :value="listFont.usingCustom.value ? CUSTOM_FONT_ID : listFontFamily"
                @change="onFontSelect($event, listFont, 'listFontFamily')"
              >
                <option v-for="option in LIST_FONT_OPTIONS" :key="option.id" :value="option.id">
                  {{ option.label }}
                </option>
                <option :value="CUSTOM_FONT_ID">Custom…</option>
              </select>
            </label>
            <label v-if="listFont.usingCustom.value">
              <span class="visually-hidden">Custom table list font</span>
              <input
                type="text"
                :value="listFont.customFont.value"
                placeholder="Font family name"
                spellcheck="false"
                @change="onCustomFont($event, listFont, 'listFontFamily')"
              />
            </label>
          </div>
        </div>
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Table list size</h3>
            <p class="muted tiny">Size of the table names in the connection sidebar.</p>
          </div>
          <div class="settings-control settings-slider">
            <label class="settings-slider-input">
              <span class="visually-hidden">Table list font size</span>
              <input
                type="range"
                :min="FONT_SIZE_MIN"
                :max="FONT_SIZE_MAX"
                step="0.5"
                :value="listFontSize"
                @input="previewPreferences({ listFontSize: sliderValue($event) })"
                @change="save({ listFontSize: sliderValue($event) })"
              />
            </label>
            <span class="settings-slider-value">{{ formatFontSize(listFontSize) }}</span>
            <button
              class="ghost tiny"
              type="button"
              :disabled="listFontSize === DEFAULT_LIST_FONT_SIZE"
              @click="save({ listFontSize: DEFAULT_LIST_FONT_SIZE })"
            >
              Reset
            </button>
          </div>
        </div>
      </section>
    </div>
  </div>
</template>
