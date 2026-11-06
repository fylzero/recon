<script setup lang="ts">
import { confirm } from "@tauri-apps/plugin-dialog";
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import * as api from "../api";
import type { ImportProgress, ImportWorkerProgress, MachineStats } from "../types";
import { formatBytes } from "../transfer";

const props = defineProps<{
  progress: ImportProgress | null;
}>();

const WIDGETS_KEY = "recon.importDashboardWidgets";
const THRESHOLDS_KEY = "recon.importTempThresholds";

interface WidgetState {
  donut: boolean;
  clock: boolean;
  temperature: boolean;
  elapsed: boolean;
  cores: boolean;
  rate: boolean;
  threads: boolean;
}

interface TempThresholds {
  goodMax: number;
  warnMax: number;
}

const DEFAULT_WIDGETS: WidgetState = {
  donut: true,
  clock: true,
  temperature: true,
  elapsed: true,
  cores: true,
  rate: true,
  threads: true,
};

const DEFAULT_THRESHOLDS: TempThresholds = {
  goodMax: 60,
  warnMax: 80,
};

const WIDGET_ITEMS: { key: keyof WidgetState; label: string }[] = [
  { key: "donut", label: "Donut" },
  { key: "clock", label: "Clock" },
  { key: "temperature", label: "Temp" },
  { key: "elapsed", label: "Time" },
  { key: "cores", label: "Cores EQ" },
  { key: "rate", label: "Inserts/s" },
  { key: "threads", label: "Threads" },
];

function loadWidgets(): WidgetState {
  try {
    const raw = JSON.parse(localStorage.getItem(WIDGETS_KEY) ?? "{}");
    const next = { ...DEFAULT_WIDGETS };
    for (const key of Object.keys(next) as (keyof WidgetState)[]) {
      if (typeof raw?.[key] === "boolean") {
        next[key] = raw[key];
      }
    }
    return next;
  } catch {
    return { ...DEFAULT_WIDGETS };
  }
}

function loadThresholds(): TempThresholds {
  try {
    const raw = JSON.parse(localStorage.getItem(THRESHOLDS_KEY) ?? "{}");
    const goodMax = typeof raw?.goodMax === "number" && Number.isFinite(raw.goodMax) ? raw.goodMax : DEFAULT_THRESHOLDS.goodMax;
    const warnMax = typeof raw?.warnMax === "number" && Number.isFinite(raw.warnMax) ? raw.warnMax : DEFAULT_THRESHOLDS.warnMax;
    return {
      goodMax: Math.max(20, Math.min(110, goodMax)),
      warnMax: Math.max(goodMax + 1, Math.min(120, warnMax)),
    };
  } catch {
    return { ...DEFAULT_THRESHOLDS };
  }
}

const widgets = ref<WidgetState>(loadWidgets());
const thresholds = ref<TempThresholds>(loadThresholds());
const editingThresholds = ref(false);

watch(widgets, (val) => localStorage.setItem(WIDGETS_KEY, JSON.stringify(val)), { deep: true });
watch(
  thresholds,
  (val) => {
    if (val.warnMax <= val.goodMax) {
      val.warnMax = val.goodMax + 1;
    }
    localStorage.setItem(THRESHOLDS_KEY, JSON.stringify(val));
  },
  { deep: true },
);

function toggleWidget(key: keyof WidgetState) {
  widgets.value[key] = !widgets.value[key];
}

// Keep widget definitions and toggle helper available for future UI controls
void WIDGET_ITEMS;
void toggleWidget;

const preferenceReady = ref(false);
const granted = ref(false);
const asking = ref(false);
const stats = ref<MachineStats | null>(null);

const startedAt = ref(Date.now());
const nowMs = ref(Date.now());
const phase1DurationMs = ref<number | null>(null);

interface RateSample {
  rate: number;
  avg: number;
}

const MAX_RATE_SAMPLES = 40;
const MOVING_AVG_WINDOW = 5;
const rateSamples = ref<RateSample[]>([]);
let lastStatements = 0;
let lastSampleMs = Date.now();

let alive = true;
let statsTimer: ReturnType<typeof setInterval> | null = null;
let tickTimer: ReturnType<typeof setInterval> | null = null;
let sampling = false;

const workers = computed(() => [...(props.progress?.workers ?? [])].sort((a, b) => a.index - b.index));
const planning = computed(() => !props.progress || props.progress.phase === "planning");

watch(
  () => props.progress?.phase,
  (phase) => {
    if (phase && phase !== "planning" && phase1DurationMs.value === null) {
      phase1DurationMs.value = Math.max(0, Date.now() - startedAt.value);
    }
  },
  { immediate: true },
);

const phase1DurationText = computed(() => {
  if (phase1DurationMs.value === null) {
    return "";
  }
  const secs = phase1DurationMs.value / 1000;
  return `${secs.toFixed(1)}s`;
});

const phase1Percent = computed(() => {
  const current = props.progress;
  if (!current) {
    return 0;
  }
  if (current.phase !== "planning") {
    return 100;
  }
  return percentOf(current.fileBytes, current.fileTotalBytes);
});

const phase2InsertCounts = computed(() => {
  const current = props.progress;
  if (!current || current.phase === "planning") {
    return { insertsRun: 0, insertsLeft: 0, totalInserts: 0 };
  }
  let insertsRun = 0;
  let totalInserts = 0;
  for (const worker of current.workers) {
    insertsRun += worker.statements;
    totalInserts += worker.totalStatements;
  }
  if (totalInserts === 0 && current.totalStatements > 0) {
    insertsRun = current.statements;
    totalInserts = current.totalStatements;
  }
  insertsRun = Math.min(totalInserts, Math.max(0, insertsRun));
  const insertsLeft = Math.max(0, totalInserts - insertsRun);
  return { insertsRun, insertsLeft, totalInserts };
});

const phase2Percent = computed(() => {
  const current = props.progress;
  if (!current || current.phase === "planning") {
    return 0;
  }
  const { insertsRun, insertsLeft } = phase2InsertCounts.value;
  return percentOf(insertsRun, insertsRun + insertsLeft);
});

const totalPercent = computed(() => (planning.value ? phase1Percent.value : phase2Percent.value));

// Concentric donut SVG geometry: center = (85, 85)
// Phase 1 = inside track (r = 59), Phase 2 = outside track (r = 72)
const DONUT_INNER_R = 59;
const DONUT_OUTER_R = 72;
const DONUT_INNER_CIRC = 2 * Math.PI * DONUT_INNER_R;
const DONUT_OUTER_CIRC = 2 * Math.PI * DONUT_OUTER_R;

const phase1ArcFill = computed(
  () => (DONUT_INNER_CIRC * Math.min(100, Math.max(0, phase1Percent.value))) / 100,
);
const phase2ArcFill = computed(
  () => (DONUT_OUTER_CIRC * Math.min(100, Math.max(0, phase2Percent.value))) / 100,
);

const elapsedSeconds = computed(() => Math.max(0, (nowMs.value - startedAt.value) / 1000));

const elapsedFormatted = computed(() => {
  const totalSec = Math.floor(elapsedSeconds.value);
  const mins = Math.floor(totalSec / 60);
  const secs = totalSec % 60;
  return `${String(mins).padStart(2, "0")}:${String(secs).padStart(2, "0")}`;
});

// Clock hand angles in degrees (0 deg = 12 o'clock)
const secondHandAngle = computed(() => (elapsedSeconds.value % 60) * 6);
const minuteHandAngle = computed(() => ((elapsedSeconds.value / 60) % 60) * 6);

const fileLabel = computed(() => {
  const current = props.progress;
  if (!current) {
    return "";
  }
  return `${formatBytes(current.fileBytes)} / ${formatBytes(current.fileTotalBytes)}`;
});

const statementLabel = computed(() => {
  const current = props.progress;
  if (!current) {
    return "";
  }
  return `${current.statements.toLocaleString()} / ${current.totalStatements.toLocaleString()} statements`;
});
void fileLabel;
void statementLabel;

const temperatureValue = computed(() => {
  const value = stats.value?.temperatureC;
  return typeof value === "number" && Number.isFinite(value) ? value : null;
});

const temperatureText = computed(() => {
  if (temperatureValue.value === null) {
    return "Unavailable";
  }
  return `${temperatureValue.value.toLocaleString(undefined, { maximumFractionDigits: 1 })}°C`;
});

const temperatureLevel = computed<"good" | "warn" | "bad" | "muted">(() => {
  const val = temperatureValue.value;
  if (val === null) {
    return "muted";
  }
  if (val < thresholds.value.goodMax) {
    return "good";
  }
  if (val <= thresholds.value.warnMax) {
    return "warn";
  }
  return "bad";
});

const cores = computed(() => (Array.isArray(stats.value?.load) ? stats.value.load : null));
const EQ_SEGMENTS = 14;

function eqSegmentActive(load: number, segmentFromBottom: number) {
  const clamped = clampPercent(load);
  if (clamped <= 0) {
    return false;
  }
  const litCount = Math.max(1, Math.round((clamped / 100) * EQ_SEGMENTS));
  return segmentFromBottom <= litCount;
}

function eqSegmentTier(segmentFromBottom: number): "good" | "warn" | "bad" {
  const pct = (segmentFromBottom / EQ_SEGMENTS) * 100;
  if (pct <= 60) {
    return "good";
  }
  if (pct <= 82) {
    return "warn";
  }
  return "bad";
}

// Live inserts/sec line graph geometry
const CHART_W = 420;
const CHART_H = 74;

const currentRate = computed(() => {
  const last = rateSamples.value[rateSamples.value.length - 1];
  return last ? Math.round(last.rate) : 0;
});

const currentMovingAvg = computed(() => {
  const last = rateSamples.value[rateSamples.value.length - 1];
  return last ? Math.round(last.avg) : 0;
});

const peakRate = computed(() => {
  let max = 10;
  for (const s of rateSamples.value) {
    if (s.rate > max) max = s.rate;
    if (s.avg > max) max = s.avg;
  }
  return max;
});

function buildPolyline(key: "rate" | "avg"): string {
  const list = rateSamples.value;
  if (!list.length) {
    return "";
  }
  const max = peakRate.value;
  const padY = 6;
  const usableH = CHART_H - padY * 2;
  if (list.length === 1) {
    const y = ( padY + usableH * (1 - list[0][key] / max) ).toFixed(1);
    return `0,${y} ${CHART_W},${y}`;
  }
  return list
    .map((sample, idx) => {
      const x = ((idx / (list.length - 1)) * CHART_W).toFixed(1);
      const y = (padY + usableH * (1 - Math.min(1, Math.max(0, sample[key] / max)))).toFixed(1);
      return `${x},${y}`;
    })
    .join(" ");
}

const rateLinePoints = computed(() => buildPolyline("rate"));
const avgLinePoints = computed(() => buildPolyline("avg"));
const rateAreaPoints = computed(() => {
  const line = rateLinePoints.value;
  if (!line) {
    return "";
  }
  return `0,${CHART_H} ${line} ${CHART_W},${CHART_H}`;
});

function percentOf(done: number, total: number) {
  if (!total) {
    return 0;
  }
  return Math.min(100, Math.max(0, (done / total) * 100));
}

function clampPercent(value: number) {
  if (!Number.isFinite(value)) {
    return 0;
  }
  return Math.min(100, Math.max(0, value));
}

function formatLoad(value: number) {
  if (!Number.isFinite(value)) {
    return "0%";
  }
  return `${Math.round(clampPercent(value))}%`;
}

function stateLabel(state: ImportWorkerProgress["state"]) {
  if (state === "waiting") {
    return "Waiting";
  }
  if (state === "done") {
    return "Done";
  }
  return "Running";
}

function sampleRate() {
  const now = Date.now();
  nowMs.value = now;
  const dt = (now - lastSampleMs) / 1000;
  if (dt < 0.2) {
    return;
  }
  const currentStmts = phase2InsertCounts.value.insertsRun;
  const delta = Math.max(0, currentStmts - lastStatements);
  const rate = dt > 0 ? delta / dt : 0;
  lastStatements = currentStmts;
  lastSampleMs = now;

  const history = rateSamples.value;
  const recentRates = history.slice(-(MOVING_AVG_WINDOW - 1)).map((item) => item.rate);
  recentRates.push(rate);
  const avg = recentRates.reduce((sum, v) => sum + v, 0) / recentRates.length;

  const next = [...history, { rate, avg }];
  if (next.length > MAX_RATE_SAMPLES) {
    next.splice(0, next.length - MAX_RATE_SAMPLES);
  }
  rateSamples.value = next;
}

function stopPoll() {
  if (statsTimer) {
    clearInterval(statsTimer);
    statsTimer = null;
  }
  if (tickTimer) {
    clearInterval(tickTimer);
    tickTimer = null;
  }
}

async function sample() {
  if (!alive || sampling) {
    return;
  }
  sampling = true;
  try {
    const next = await api.machineStats();
    if (alive) {
      stats.value = next;
    }
  } catch {
  } finally {
    sampling = false;
  }
}

function startStatsPoll() {
  if (statsTimer) {
    return;
  }
  void sample();
  statsTimer = setInterval(() => {
    void sample();
  }, 1000);
}

async function requestStats() {
  if (granted.value || asking.value) {
    return;
  }
  asking.value = true;
  try {
    const ok = await confirm("Show this machine's processor load and temperature while an import or restore runs?", {
      title: "Machine stats",
      kind: "info",
      okLabel: "Grant",
      cancelLabel: "Don't allow",
    });
    if (!ok || !alive) {
      return;
    }
    await api.updatePreferences({ machineStats: true });
    if (!alive) {
      return;
    }
    granted.value = true;
    startStatsPoll();
  } catch {
  } finally {
    asking.value = false;
  }
}

onMounted(() => {
  startedAt.value = Date.now();
  nowMs.value = startedAt.value;
  lastSampleMs = startedAt.value;
  lastStatements = phase2InsertCounts.value.insertsRun;
  rateSamples.value = [{ rate: 0, avg: 0 }];

  tickTimer = setInterval(() => {
    sampleRate();
  }, 500);

  void api
    .getState()
    .then((data) => {
      if (!alive) {
        return;
      }
      preferenceReady.value = true;
      if (data.machineStats) {
        granted.value = true;
        startStatsPoll();
      }
    })
    .catch(() => {
      if (alive) {
        preferenceReady.value = true;
      }
    });
});

onUnmounted(() => {
  alive = false;
  stopPoll();
});
</script>

<template>
  <div class="import-work">
    <div v-if="preferenceReady && !granted" class="import-grant-row">
      <button
        class="ghost import-work-grant"
        type="button"
        :disabled="asking"
        @click="requestStats"
      >
        Grant CPU &amp; temp stats
      </button>
    </div>

    <div class="import-dashboard-grid" :class="{ 'no-left-column': !widgets.donut && !widgets.clock }">
      <!-- Left Column: Concentric Phase 1 (Inside) / Phase 2 (Outside) Donut + Center Clock Face -->
      <div v-if="widgets.donut || widgets.clock" class="import-donut-column">
        <div class="import-donut-wrap">
          <svg viewBox="0 0 170 170" class="import-donut-svg" aria-label="Import progress donut">
            <template v-if="widgets.donut">
              <!-- Phase 2: Outside Track -->
              <circle
                cx="85"
                cy="85"
                :r="DONUT_OUTER_R"
                class="import-donut-track import-donut-track-outer"
              />
              <circle
                cx="85"
                cy="85"
                :r="DONUT_OUTER_R"
                class="import-donut-fill import-donut-fill-outer"
                :stroke-dasharray="`${phase2ArcFill} ${DONUT_OUTER_CIRC}`"
                transform="rotate(-90 85 85)"
              />

              <!-- Phase 1: Inside Track -->
              <circle
                cx="85"
                cy="85"
                :r="DONUT_INNER_R"
                class="import-donut-track import-donut-track-inner"
              />
              <circle
                cx="85"
                cy="85"
                :r="DONUT_INNER_R"
                class="import-donut-fill import-donut-fill-inner"
                :class="{ done: !planning || phase1Percent >= 100 }"
                :stroke-dasharray="`${phase1ArcFill} ${DONUT_INNER_CIRC}`"
                transform="rotate(-90 85 85)"
              />
            </template>

            <!-- Clock Face in Donut Center -->
            <g v-if="widgets.clock" class="import-clock-face">
              <circle cx="85" cy="85" r="46" class="import-clock-ring" />
              <line
                v-for="tick in 12"
                :key="tick"
                x1="85"
                y1="42"
                x2="85"
                :y2="tick % 3 === 0 ? 47 : 45"
                class="import-clock-tick"
                :transform="`rotate(${tick * 30} 85 85)`"
              />
              <!-- Minute Hand -->
              <line
                x1="85"
                y1="85"
                x2="85"
                y2="56"
                class="import-clock-hand-min"
                :transform="`rotate(${minuteHandAngle} 85 85)`"
              />
              <!-- Second Hand -->
              <line
                x1="85"
                y1="89"
                x2="85"
                y2="49"
                class="import-clock-hand-sec"
                :transform="`rotate(${secondHandAngle} 85 85)`"
              />
              <circle cx="85" cy="85" r="2.8" class="import-clock-pin" />
              <text x="85" y="106" text-anchor="middle" class="import-clock-digital">
                {{ elapsedFormatted }}
              </text>
            </g>
            <g v-else class="import-clock-face">
              <text x="85" y="90" text-anchor="middle" class="import-donut-pct-only">
                {{ Math.round(totalPercent) }}%
              </text>
            </g>
          </svg>
        </div>

        <div v-if="widgets.donut" class="import-donut-legend">
          <div class="import-donut-phase" :class="{ active: planning, done: !planning || phase1Percent >= 100 }">
            <span class="import-phase-dot" />
            <div class="import-phase-text">
              <span class="import-phase-title">Phase 1</span>
              <span class="import-phase-desc">Create tables</span>
            </div>
            <div class="import-phase-stat">
              <strong>{{ Math.round(phase1Percent) }}%</strong>
              <span v-if="phase1DurationText" class="import-phase-time">{{ phase1DurationText }}</span>
            </div>
          </div>
          <div class="import-donut-phase" :class="{ active: !planning, done: phase2Percent >= 100 }">
            <span class="import-phase-dot" />
            <div class="import-phase-text">
              <span class="import-phase-title">Phase 2</span>
              <span class="import-phase-desc">Insert rows</span>
            </div>
            <div class="import-phase-stat">
              <strong>{{ Math.round(phase2Percent) }}%</strong>
            </div>
          </div>
        </div>
      </div>

      <!-- Right Column: Temperature, Time Spent, Equalizer Cores, Live Inserts/s Graph, Per-Thread Bars -->
      <div class="import-metrics-column">
        <!-- Top Metric Cards Row -->
        <div v-if="widgets.temperature || widgets.elapsed" class="import-top-cards">
          <!-- Temperature Card with Adjustable Thresholds -->
          <div v-if="widgets.temperature" class="import-metric-card">
            <div class="import-metric-head">
              <span class="import-work-k">CPU Temperature</span>
              <button
                type="button"
                class="import-threshold-btn"
                :title="`Thresholds: Green < ${thresholds.goodMax}°C, Yellow ${thresholds.goodMax}–${thresholds.warnMax}°C, Red > ${thresholds.warnMax}°C`"
                @click="editingThresholds = !editingThresholds"
              >
                &lt;{{ thresholds.goodMax }}° / {{ thresholds.warnMax }}°C ⚙
              </button>
            </div>
            <div class="import-metric-value" :class="`temp-${temperatureLevel}`">
              {{ granted ? temperatureText : "Grant stats" }}
              <span v-if="granted && temperatureValue !== null" class="import-temp-badge" :class="`temp-${temperatureLevel}`">
                {{ temperatureLevel === "good" ? "Good" : temperatureLevel === "warn" ? "Warm" : "Hot" }}
              </span>
            </div>
            <div v-if="editingThresholds" class="import-threshold-editor">
              <label>
                <span class="temp-good">Green &lt;</span>
                <input v-model.number="thresholds.goodMax" type="number" min="25" max="105" step="1" />
                <span>°C</span>
              </label>
              <label>
                <span class="temp-bad">Red &gt;</span>
                <input v-model.number="thresholds.warnMax" type="number" min="35" max="120" step="1" />
                <span>°C</span>
              </label>
            </div>
          </div>

          <!-- Time Spent & Overall Throughput Card -->
          <div v-if="widgets.elapsed" class="import-metric-card">
            <div class="import-metric-head">
              <span class="import-work-k">Time Spent</span>
              <span class="import-work-k">{{ planning ? "Phase 1 · Create tables" : "Phase 2 · Insert rows" }}</span>
            </div>
            <div class="import-metric-value">
              {{ elapsedFormatted }}
              <span class="import-metric-sub">{{ Math.round(totalPercent) }}% complete</span>
            </div>
          </div>
        </div>

        <!-- Per-Core Equalizer Vertical Bar Graphs -->
        <div v-if="widgets.cores" class="import-widget-box">
          <div class="import-widget-header">
            <span class="import-work-k">Per-Core CPU Performance</span>
            <span v-if="cores" class="import-work-k">{{ cores.length }} logical cores</span>
            <span v-else class="import-work-k">{{ granted ? "Sampling…" : "Stats permission required" }}</span>
          </div>
          <div v-if="cores && cores.length" class="import-eq-grid">
            <div v-for="(load, index) in cores" :key="index" class="import-eq-column" :title="`Core ${index + 1}: ${formatLoad(load)}`">
              <div class="import-eq-bars">
                <span
                  v-for="seg in EQ_SEGMENTS"
                  :key="seg"
                  class="import-eq-seg"
                  :class="[
                    `tier-${eqSegmentTier(EQ_SEGMENTS - seg + 1)}`,
                    { lit: eqSegmentActive(load, EQ_SEGMENTS - seg + 1) },
                  ]"
                />
              </div>
              <span class="import-eq-pct">{{ formatLoad(load) }}</span>
              <span class="import-eq-label">C{{ index + 1 }}</span>
            </div>
          </div>
          <p v-else class="import-work-empty muted tiny">
            {{ granted ? "Waiting for core telemetry…" : "Click “Grant CPU & temp stats” above to view live per-core equalizer load." }}
          </p>
        </div>

        <!-- Live Line Graph of Inserts per Second + Dotted Moving Average -->
        <div v-if="widgets.rate" class="import-widget-box">
          <div class="import-widget-header">
            <span class="import-work-k">Inserts / sec</span>
            <div class="import-chart-legend">
              <span class="import-legend-item">
                <span class="import-legend-swatch solid" />
                {{ currentRate.toLocaleString() }}/s
              </span>
              <span class="import-legend-item">
                <span class="import-legend-swatch dotted" />
                Avg {{ currentMovingAvg.toLocaleString() }}/s
              </span>
            </div>
          </div>
          <svg :viewBox="`0 0 ${CHART_W} ${CHART_H}`" preserveAspectRatio="none" class="import-rate-svg">
            <defs>
              <linearGradient id="importRateGrad" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stop-color="#3dd68c" stop-opacity="0.28" />
                <stop offset="100%" stop-color="#3dd68c" stop-opacity="0.0" />
              </linearGradient>
            </defs>
            <polygon v-if="rateAreaPoints" :points="rateAreaPoints" fill="url(#importRateGrad)" />
            <polyline v-if="rateLinePoints" :points="rateLinePoints" class="import-rate-line" />
            <polyline v-if="avgLinePoints" :points="avgLinePoints" class="import-rate-avg-line" />
          </svg>
        </div>

        <!-- Per-Thread Progress Bars Below Core Bars -->
        <div v-if="widgets.threads && workers.length" class="import-widget-box">
          <div class="import-widget-header">
            <span class="import-work-k">Worker Threads ({{ workers.length }})</span>
          </div>
          <div class="import-work-workers">
            <div v-for="worker in workers" :key="worker.index" class="import-work-worker">
              <span>Thread {{ worker.index + 1 }}</span>
              <div class="import-work-bar">
                <span :style="{ width: `${percentOf(worker.statements, worker.totalStatements)}%` }" />
              </div>
              <span>{{ worker.statements.toLocaleString() }} / {{ worker.totalStatements.toLocaleString() }}</span>
              <span class="import-work-state" :class="`state-${worker.state}`">{{ stateLabel(worker.state) }}</span>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
