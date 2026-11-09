import { computed, ref, watch } from "vue";
import { loadJobPercent, loadRingPercents, sqlLoadPhase, type SqlLoadPhase } from "../transfer";
import type { ImportProgress } from "../types";

/** Tracks the SQL load stage across overlay phases and the shared three-phase percents. */
export function useLoadProgress(progress: () => ImportProgress | null) {
  const sqlPhase = ref<SqlLoadPhase>("planning");
  /** Last non-`reconnecting` payload. Overlay events rebuild empty 0/0 workers and must not drive rings. */
  const sqlProgress = ref<ImportProgress | null>(null);

  watch(
    progress,
    (current) => {
      sqlPhase.value = sqlLoadPhase(current, sqlPhase.value);
      if (current && current.phase !== "reconnecting") {
        sqlProgress.value = current;
      }
    },
    { immediate: true, flush: "sync" },
  );

  const counted = () => sqlProgress.value ?? progress();
  const rings = computed(() => loadRingPercents(counted(), sqlPhase.value));
  const percent = computed(() => loadJobPercent(counted(), sqlPhase.value));

  return { sqlPhase, rings, percent, sqlProgress };
}
