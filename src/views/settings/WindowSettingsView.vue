<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from "vue";
import { getCurrentWindow } from "@tauri-apps/api/window";
import * as api from "../../api";
import { useApp } from "../../composables/useApp";
import type { WindowState } from "../../types";
import {
  DEFAULT_WINDOW_HEIGHT,
  DEFAULT_WINDOW_WIDTH,
  MIN_WINDOW_HEIGHT,
  MIN_WINDOW_WIDTH,
} from "../../types";

const { windowState, saveWindowState, showToast } = useApp();

const windowDraft = ref<WindowState>({
  x: 0,
  y: 0,
  width: DEFAULT_WINDOW_WIDTH,
  height: DEFAULT_WINDOW_HEIGHT,
  maximized: false,
});

function assignWindow(next: WindowState) {
  windowDraft.value = {
    x: next.x,
    y: next.y,
    width: next.width,
    height: next.height,
    maximized: Boolean(next.maximized),
  };
}

async function loadWindow() {
  try {
    assignWindow(windowState.value ?? (await api.getWindowState()));
  } catch (err) {
    showToast(String(err), "error");
  }
}

async function persistWindow(next: WindowState) {
  try {
    assignWindow(await saveWindowState(next));
  } catch (err) {
    showToast(String(err), "error");
  }
}

function numberFromInput(event: Event, fallback: number) {
  const value = Number((event.target as HTMLInputElement).value);
  return Number.isFinite(value) ? value : fallback;
}

let stopLiveWindow: (() => void) | undefined;
let liveWindowTimer: ReturnType<typeof setTimeout> | null = null;

async function syncLiveWindow() {
  try {
    const win = getCurrentWindow();
    const maximized = await win.isMaximized();
    if (maximized) {
      if (!windowDraft.value.maximized) {
        assignWindow({ ...windowDraft.value, maximized: true });
      }
      return;
    }
    const scale = await win.scaleFactor();
    const size = (await win.innerSize()).toLogical(scale);
    const pos = (await win.outerPosition()).toLogical(scale);
    assignWindow({
      x: Math.round(pos.x),
      y: Math.round(pos.y),
      width: Math.max(MIN_WINDOW_WIDTH, Math.round(size.width)),
      height: Math.max(MIN_WINDOW_HEIGHT, Math.round(size.height)),
      maximized: false,
    });
  } catch {
    /* the window can close while a move is in flight */
  }
}

function queueLiveWindow() {
  if (liveWindowTimer !== null) {
    return;
  }
  liveWindowTimer = setTimeout(() => {
    liveWindowTimer = null;
    void syncLiveWindow();
  }, 50);
}

onMounted(() => {
  void loadWindow();
  try {
    const win = getCurrentWindow();
    void Promise.all([win.onMoved(queueLiveWindow), win.onResized(queueLiveWindow)]).then(
      (stoppers) => {
        stopLiveWindow = () => {
          for (const stop of stoppers) {
            stop();
          }
        };
      },
    );
  } catch {
    /* window listeners are only available in the native shell */
  }
});

onUnmounted(() => {
  stopLiveWindow?.();
  if (liveWindowTimer !== null) {
    clearTimeout(liveWindowTimer);
    liveWindowTimer = null;
  }
});

watch(windowState, (next) => {
  if (next) {
    assignWindow(next);
  }
});

async function onWindowWidth(event: Event) {
  const width = Math.max(MIN_WINDOW_WIDTH, Math.round(numberFromInput(event, windowDraft.value.width)));
  await persistWindow({ ...windowDraft.value, width, maximized: false });
}

async function onWindowHeight(event: Event) {
  const height = Math.max(
    MIN_WINDOW_HEIGHT,
    Math.round(numberFromInput(event, windowDraft.value.height)),
  );
  await persistWindow({ ...windowDraft.value, height, maximized: false });
}

async function onWindowX(event: Event) {
  const x = Math.round(numberFromInput(event, windowDraft.value.x));
  await persistWindow({ ...windowDraft.value, x, maximized: false });
}

async function onWindowY(event: Event) {
  const y = Math.round(numberFromInput(event, windowDraft.value.y));
  await persistWindow({ ...windowDraft.value, y, maximized: false });
}

async function onMaximized(maximized: boolean) {
  await persistWindow({ ...windowDraft.value, maximized });
}

async function resetWindow() {
  await persistWindow({
    ...windowDraft.value,
    width: DEFAULT_WINDOW_WIDTH,
    height: DEFAULT_WINDOW_HEIGHT,
    maximized: false,
  });
}
</script>

<template>
  <div class="settings-pane settings-form-page">
    <div class="settings-inner">
      <div class="settings-header">
        <div>
          <div class="brand">Window</div>
          <p class="muted tiny">Size and position follow the window as you move or resize it.</p>
        </div>
      </div>

      <section class="settings-card">
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Size</h3>
            <p class="muted tiny">
              Minimum {{ MIN_WINDOW_WIDTH }}×{{ MIN_WINDOW_HEIGHT }}. These numbers follow the window
              as you move or resize it.
            </p>
          </div>
          <div class="settings-fields">
            <label class="settings-field">
              <span>Width</span>
              <input
                type="number"
                :min="MIN_WINDOW_WIDTH"
                step="1"
                :value="windowDraft.width"
                @change="onWindowWidth"
              />
            </label>
            <label class="settings-field">
              <span>Height</span>
              <input
                type="number"
                :min="MIN_WINDOW_HEIGHT"
                step="1"
                :value="windowDraft.height"
                @change="onWindowHeight"
              />
            </label>
          </div>
        </div>
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Position</h3>
            <p class="muted tiny">Top-left of the window, in pixels.</p>
          </div>
          <div class="settings-fields">
            <label class="settings-field">
              <span>X</span>
              <input type="number" step="1" :value="windowDraft.x" @change="onWindowX" />
            </label>
            <label class="settings-field">
              <span>Y</span>
              <input type="number" step="1" :value="windowDraft.y" @change="onWindowY" />
            </label>
          </div>
        </div>
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Maximized</h3>
            <p class="muted tiny">Fill the current display. Size and position are kept for restore.</p>
          </div>
          <button
            class="history-switch"
            :class="{ on: windowDraft.maximized }"
            type="button"
            role="switch"
            :aria-checked="windowDraft.maximized"
            @click="onMaximized(!windowDraft.maximized)"
          >
            <span class="history-switch-track" aria-hidden="true">
              <span class="history-switch-knob" />
            </span>
            {{ windowDraft.maximized ? "On" : "Off" }}
          </button>
        </div>
        <div class="settings-row">
          <div class="settings-row-copy">
            <h3>Reset size</h3>
            <p class="muted tiny">
              Restore the default {{ DEFAULT_WINDOW_WIDTH }}×{{ DEFAULT_WINDOW_HEIGHT }} window.
              Position is kept.
            </p>
          </div>
          <button class="ghost" type="button" @click="resetWindow">Reset</button>
        </div>
      </section>
    </div>
  </div>
</template>
