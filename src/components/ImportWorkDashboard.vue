<script setup lang="ts">
import { confirm } from "@tauri-apps/plugin-dialog";
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import * as api from "../api";
import { useApp } from "../composables/useApp";
import { hasMachineStats } from "../platform";
import { importStages, stagesPercent } from "../transfer";
import type { ImportProgress, ImportWorkerProgress, MachineStats } from "../types";

const props = defineProps<{
  progress: ImportProgress | null;
}>();

const THRESHOLDS_KEY = "recon.importTempThresholds";

interface TempThresholds {
  goodMax: number;
  warnMax: number;
}

const DEFAULT_THRESHOLDS: TempThresholds = {
  goodMax: 60,
  warnMax: 80,
};

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

const thresholds = ref<TempThresholds>(loadThresholds());
const editingThresholds = ref(false);

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

const { machineStats: granted, temperatureUnit, savePreferences, showToast } = useApp();

const celsius = computed(() => temperatureUnit.value === "celsius");

const unit = computed(() => (celsius.value ? "°C" : "°F"));

/** `degrees` Celsius in the unit the user reads temperatures in. */
function shown(degrees: number) {
  return celsius.value ? degrees : (degrees * 9) / 5 + 32;
}

function fromShown(value: number) {
  return celsius.value ? value : ((value - 32) * 5) / 9;
}

/** The thresholds stay in Celsius, so changing the unit doesn't move them. */
function thresholdField(key: keyof TempThresholds) {
  return computed({
    get: () => Math.round(shown(thresholds.value[key])),
    set: (value: number) => {
      if (Number.isFinite(value)) {
        thresholds.value[key] = fromShown(value);
      }
    },
  });
}

const goodMaxShown = thresholdField("goodMax");
const warnMaxShown = thresholdField("warnMax");
const asking = ref(false);
const stats = ref<MachineStats | null>(null);

const startedAt = ref(Date.now());
const nowMs = ref(Date.now());
/** When each stage finished, in ms since the run started. */
const stageEnds = ref<Record<string, number>>({});

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
const stages = computed(() => importStages(props.progress));
const activeStage = computed(() => stages.value.find((stage) => stage.percent < 100) ?? null);
const totalPercent = computed(() => stagesPercent(stages.value));

watch(
  stages,
  (list) => {
    for (const stage of list) {
      if (stage.percent >= 100 && stageEnds.value[stage.id] === undefined) {
        stageEnds.value = { ...stageEnds.value, [stage.id]: Math.max(0, Date.now() - startedAt.value) };
      }
    }
  },
  { immediate: true },
);

function stageTime(index: number) {
  const end = stageEnds.value[stages.value[index].id];
  if (end === undefined) {
    return "";
  }
  const previous = index > 0 ? (stageEnds.value[stages.value[index - 1].id] ?? 0) : 0;
  return `${(Math.max(0, end - previous) / 1000).toFixed(1)}s`;
}

// One ring around the clock, split into a segment per stage by weight.
const RING_R = 66;
const RING_CIRC = 2 * Math.PI * RING_R;
const RING_GAP = 14;

const segments = computed(() => {
  const list = stages.value;
  const weight = list.reduce((sum, stage) => sum + stage.weight, 0) || 1;
  const gap = list.length > 1 ? RING_GAP : 0;
  const usable = RING_CIRC - gap * list.length;
  let start = 0;
  return list.map((stage) => {
    const length = (usable * stage.weight) / weight;
    const segment = {
      stage,
      offset: -start,
      length,
      fill: (length * Math.min(100, Math.max(0, stage.percent))) / 100,
    };
    start += length + gap;
    return segment;
  });
});

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

const temperatureValue = computed(() => {
  const value = stats.value?.temperatureCelsius;
  return typeof value === "number" && Number.isFinite(value) ? value : null;
});

const temperatureText = computed(() => {
  if (temperatureValue.value === null) {
    return "Unavailable";
  }
  return `${shown(temperatureValue.value).toLocaleString(undefined, { maximumFractionDigits: 1 })}${unit.value}`;
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

const cores = computed(() => {
  const list = stats.value?.cores;
  return Array.isArray(list) && list.length ? list : null;
});
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
  const currentStmts = props.progress?.statements ?? 0;
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

function stopStatsPoll() {
  if (statsTimer) {
    clearInterval(statsTimer);
    statsTimer = null;
  }
  stats.value = null;
}

function stopPoll() {
  stopStatsPoll();
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
  } catch (err) {
    stopStatsPoll();
    showToast(`Could not read machine stats: ${String(err)}`, "error");
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
    const ok = await confirm("Show this machine's processor load and temperature while an import or restore runs? You can turn this off in Settings → General.", {
      title: "Machine stats",
      kind: "info",
      okLabel: "Grant",
      cancelLabel: "Don't allow",
    });
    if (ok) {
      await savePreferences({ machineStats: true });
    }
  } catch (err) {
    showToast(`Could not turn on machine stats: ${String(err)}`, "error");
  } finally {
    asking.value = false;
  }
}

watch(granted, (on) => {
  if (!alive) {
    return;
  }
  if (on && hasMachineStats) {
    startStatsPoll();
  } else {
    stopStatsPoll();
  }
});

onMounted(() => {
  startedAt.value = Date.now();
  nowMs.value = startedAt.value;
  lastSampleMs = startedAt.value;
  lastStatements = props.progress?.statements ?? 0;
  rateSamples.value = [{ rate: 0, avg: 0 }];

  tickTimer = setInterval(() => {
    sampleRate();
  }, 500);

  if (granted.value && hasMachineStats) {
    startStatsPoll();
  }
});

onUnmounted(() => {
  alive = false;
  stopPoll();
});
</script>

<template>
  <div class="import-work">
    <div v-if="hasMachineStats && !granted" class="import-grant-row">
      <button
        class="ghost import-work-grant"
        type="button"
        :disabled="asking"
        @click="requestStats"
      >
        Grant CPU &amp; temp stats
      </button>
    </div>

    <div class="import-dashboard-grid">
      <!-- Left Column: Concentric Phase 1 (Inside) / Phase 2 (Outside) Donut + Center Clock Face -->
      <div class="import-donut-column">
        <div class="import-donut-wrap">
          <svg viewBox="0 0 170 170" class="import-donut-svg" aria-label="Import progress donut">
            <g v-for="segment in segments" :key="segment.stage.id" transform="rotate(-90 85 85)">
              <circle
                cx="85"
                cy="85"
                :r="RING_R"
                class="import-donut-track"
                :stroke-dasharray="`${segment.length} ${RING_CIRC}`"
                :stroke-dashoffset="segment.offset"
              />
              <circle
                v-if="segment.fill > 0"
                cx="85"
                cy="85"
                :r="RING_R"
                class="import-donut-fill"
                :class="{ done: segment.stage.percent >= 100 }"
                :stroke-dasharray="`${segment.fill} ${RING_CIRC}`"
                :stroke-dashoffset="segment.offset"
              />
            </g>

            <!-- Clock Face in Donut Center -->
            <g class="import-clock-face">
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
          </svg>
        </div>

        <div class="import-donut-legend">
          <div
            v-for="(stage, index) in stages"
            :key="stage.id"
            class="import-donut-phase"
            :class="{ active: stage === activeStage, done: stage.percent >= 100 }"
          >
            <span class="import-phase-dot" />
            <div class="import-phase-text">
              <span class="import-phase-title">{{ stage.title }}</span>
              <span v-if="stage.description" class="import-phase-desc">{{ stage.description }}</span>
            </div>
            <div class="import-phase-stat">
              <strong>{{ Math.round(stage.percent) }}%</strong>
              <span v-if="stageTime(index)" class="import-phase-time">{{ stageTime(index) }}</span>
            </div>
          </div>
          <div v-if="progress" class="import-donut-facts">
            <div>
              <span>Connections</span>
              <span>{{ Math.max(1, progress.workers.length) }}</span>
            </div>
            <div v-if="progress.statements > 0">
              <span>Statements</span>
              <span>{{ progress.statements.toLocaleString() }}</span>
            </div>
          </div>
        </div>
      </div>

      <!-- Right Column: Temperature, Time Spent, Equalizer Cores, Live Inserts/s Graph, Per-Thread Bars -->
      <div class="import-metrics-column">
        <!-- Top Metric Cards Row -->
        <div class="import-top-cards">
          <!-- Temperature Card with Adjustable Thresholds -->
          <div v-if="hasMachineStats" class="import-metric-card">
            <div class="import-metric-head">
              <span class="import-work-k">CPU Temperature</span>
              <button
                type="button"
                class="import-threshold-btn"
                :title="`Thresholds: Green < ${goodMaxShown}${unit}, Yellow ${goodMaxShown}–${warnMaxShown}${unit}, Red > ${warnMaxShown}${unit}`"
                @click="editingThresholds = !editingThresholds"
              >
                &lt;{{ goodMaxShown }}° / {{ warnMaxShown }}{{ unit }} ⚙
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
                <input v-model.number="goodMaxShown" type="number" step="1" />
                <span>{{ unit }}</span>
              </label>
              <label>
                <span class="temp-bad">Red &gt;</span>
                <input v-model.number="warnMaxShown" type="number" step="1" />
                <span>{{ unit }}</span>
              </label>
            </div>
          </div>

          <!-- Time Spent & Overall Throughput Card -->
          <div class="import-metric-card">
            <div class="import-metric-head">
              <span class="import-work-k">Time Spent</span>
              <span class="import-work-k">{{ activeStage?.title ?? "Finishing" }}</span>
            </div>
            <div class="import-metric-value">
              {{ elapsedFormatted }}
              <span class="import-metric-sub">{{ Math.round(totalPercent) }}% complete</span>
            </div>
          </div>
        </div>

        <!-- Per-Core Equalizer Vertical Bar Graphs -->
        <div v-if="hasMachineStats" class="import-widget-box">
          <div class="import-widget-header">
            <span class="import-work-k">CPU Performance</span>
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
              <span class="import-eq-label">{{ index + 1 }}</span>
            </div>
          </div>
          <p v-else class="import-work-empty muted tiny">
            {{ granted ? "Waiting for core telemetry…" : "Click “Grant CPU & temp stats” above to view live per-core equalizer load." }}
          </p>
        </div>

        <!-- Live Line Graph of Inserts per Second + Dotted Moving Average -->
        <div class="import-widget-box">
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
        <div v-if="workers.length" class="import-widget-box">
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
