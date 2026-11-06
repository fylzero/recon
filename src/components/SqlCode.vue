<script setup lang="ts">
import { sql, type SQLNamespace } from "@codemirror/lang-sql";
import { Compartment, EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { basicSetup } from "codemirror";
import { onMounted, onUnmounted, ref, watch } from "vue";
import { sqlDialect, sqlEditorTheme, sqlHighlighting } from "../sqlEditor";
import type { Driver } from "../types";

const props = defineProps<{
  modelValue: string;
  driver: Driver;
  schema?: SQLNamespace;
  readonly?: boolean;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: string];
}>();

const readonlyTheme = EditorView.theme({
  ".cm-activeLine, .cm-activeLineGutter": { backgroundColor: "transparent" },
});

const host = ref<HTMLDivElement | null>(null);
const language = new Compartment();
const access = new Compartment();
let view: EditorView | null = null;

function languageExtension() {
  return sql({ dialect: sqlDialect(props.driver), schema: props.schema, upperCaseKeywords: true });
}

function accessExtension() {
  return props.readonly ? [EditorState.readOnly.of(true), EditorView.editable.of(false), readonlyTheme] : [];
}

function focus() {
  view?.focus();
}

watch(
  () => props.modelValue,
  (value) => {
    if (view && value !== view.state.doc.toString()) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: value } });
    }
  },
);

watch([() => props.driver, () => props.schema], () => {
  view?.dispatch({ effects: language.reconfigure(languageExtension()) });
});

watch(
  () => props.readonly,
  () => {
    view?.dispatch({ effects: access.reconfigure(accessExtension()) });
  },
);

onMounted(() => {
  if (!host.value) {
    return;
  }
  view = new EditorView({
    parent: host.value,
    state: EditorState.create({
      doc: props.modelValue,
      extensions: [
        basicSetup,
        language.of(languageExtension()),
        access.of(accessExtension()),
        sqlEditorTheme,
        sqlHighlighting,
        EditorView.updateListener.of((update) => {
          if (update.docChanged) {
            emit("update:modelValue", update.state.doc.toString());
          }
        }),
      ],
    }),
  });
});

onUnmounted(() => {
  view?.destroy();
  view = null;
});

defineExpose({ focus });
</script>

<template>
  <div ref="host" class="sql-code" />
</template>
