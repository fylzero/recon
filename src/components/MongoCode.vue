<script setup lang="ts">
import { json } from "@codemirror/lang-json";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { EditorState, Prec } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";
import { basicSetup } from "codemirror";
import { onMounted, onUnmounted, ref, watch } from "vue";
import { sqlEditorTheme } from "../sqlEditor";

/**
 * A code editor for MongoDB documents and queries. Text is mongosh-style, so
 * it uses the JSON grammar for colors but doesn't flag ObjectId("…") and the
 * like as mistakes.
 */
const props = defineProps<{
  modelValue: string;
  readonly?: boolean;
}>();

const emit = defineEmits<{
  "update:modelValue": [value: string];
  /** ⌘S or ⌘Return. */
  submit: [];
}>();

const highlighting = syntaxHighlighting(
  HighlightStyle.define([
    { tag: t.propertyName, color: "var(--syntax-type)" },
    { tag: t.string, color: "var(--syntax-string)" },
    { tag: t.number, color: "var(--syntax-number)" },
    { tag: [t.bool, t.null], color: "var(--syntax-constant)" },
    { tag: [t.punctuation, t.brace, t.squareBracket], color: "var(--syntax-operator)" },
  ]),
);

const host = ref<HTMLDivElement | null>(null);
let view: EditorView | null = null;

watch(
  () => props.modelValue,
  (value) => {
    if (view && value !== view.state.doc.toString()) {
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: value } });
    }
  },
);

function focus() {
  view?.focus();
}

onMounted(() => {
  if (!host.value) return;
  const submit = () => {
    emit("submit");
    return true;
  };
  view = new EditorView({
    parent: host.value,
    state: EditorState.create({
      doc: props.modelValue,
      extensions: [
        Prec.highest(
          keymap.of([
            { key: "Mod-s", run: submit },
            { key: "Mod-Enter", run: submit },
          ]),
        ),
        basicSetup,
        json(),
        sqlEditorTheme,
        highlighting,
        EditorView.lineWrapping,
        ...(props.readonly ? [EditorState.readOnly.of(true), EditorView.editable.of(false)] : []),
        EditorView.updateListener.of((update) => {
          if (update.docChanged) emit("update:modelValue", update.state.doc.toString());
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
  <div ref="host" class="sql-code mongo-code" />
</template>
