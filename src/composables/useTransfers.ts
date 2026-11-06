import { computed, onUnmounted, ref } from "vue";
import * as api from "../api";

export type TransferKind = "export" | "import" | "backup" | "restore";

/** `attention` means it stopped with something to read, like an error, and the dialog is waiting. */
export type TransferStatus = "running" | "attention";

export interface TransferJob {
  id: string;
  sessionId: string;
  kind: TransferKind;
  label: string;
  percent: number;
  status: TransferStatus;
  hidden: boolean;
}

const jobs = ref<TransferJob[]>([]);

function patch(id: string, next: Partial<TransferJob>) {
  jobs.value = jobs.value.map((job) => (job.id === id ? { ...job, ...next } : job));
}

function remove(id: string) {
  jobs.value = jobs.value.filter((job) => job.id !== id);
}

export function useTransfers() {
  function jobsFor(sessionId: string) {
    return jobs.value.filter((job) => job.sessionId === sessionId);
  }

  function runningFor(sessionId: string) {
    return jobsFor(sessionId).filter((job) => job.status === "running");
  }

  function reveal(id: string) {
    patch(id, { hidden: false });
  }

  /** Brings back a hidden dialog of this kind. False when there isn't one. */
  function revealKind(sessionId: string, kind: TransferKind) {
    const job = jobsFor(sessionId).find((item) => item.kind === kind);
    if (job) {
      reveal(job.id);
    }
    return Boolean(job);
  }

  return { jobs, jobsFor, runningFor, reveal, revealKind };
}

/**
 * Tracks the transfer a dialog runs so it can be hidden while it runs and
 * shown on its tab. Unmounting the dialog, as closing its tab does, cancels
 * the transfer.
 */
export function useTransferJob(sessionId: () => string, kind: TransferKind) {
  const id = ref("");
  const job = computed(() => jobs.value.find((item) => item.id === id.value) ?? null);
  const hidden = computed(() => job.value?.hidden ?? false);

  function begin(label: string) {
    if (id.value) {
      remove(id.value);
    }
    id.value = crypto.randomUUID();
    jobs.value = [
      ...jobs.value,
      { id: id.value, sessionId: sessionId(), kind, label, percent: 0, status: "running", hidden: false },
    ];
    return id.value;
  }

  function progress(percent: number) {
    if (job.value && job.value.percent !== percent) {
      patch(id.value, { percent });
    }
  }

  /** Called when the transfer ends but the dialog stays open to show why. */
  function stopped() {
    patch(id.value, { status: "attention" });
  }

  function hide() {
    patch(id.value, { hidden: true });
  }

  onUnmounted(() => {
    if (job.value?.status === "running") {
      void api.cancelTransfer(id.value);
    }
    remove(id.value);
  });

  return { job, hidden, begin, progress, stopped, hide };
}
