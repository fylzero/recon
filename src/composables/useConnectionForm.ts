import { ref } from "vue";
import type { ConnectionEntry } from "../types";

export interface ConnectionFormState {
  groupId: string | null;
  connection: ConnectionEntry | null;
}

const formState = ref<ConnectionFormState | null>(null);

export function useConnectionForm() {
  function openNewConnection(groupId: string | null = null) {
    formState.value = { groupId, connection: null };
  }

  function openEditConnection(connection: ConnectionEntry, groupId: string | null) {
    formState.value = { groupId, connection: { ...connection } };
  }

  function closeConnectionForm() {
    formState.value = null;
  }

  return { formState, openNewConnection, openEditConnection, closeConnectionForm };
}
