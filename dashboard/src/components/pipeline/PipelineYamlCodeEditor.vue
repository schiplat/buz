<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { EditorView, keymap, lineNumbers, highlightActiveLine, highlightActiveLineGutter, drawSelection, placeholder as cmPlaceholder } from '@codemirror/view'
import { EditorState, Compartment } from '@codemirror/state'
import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands'
import { yaml } from '@codemirror/lang-yaml'
import { autocompletion, closeBrackets, closeBracketsKeymap, completionKeymap } from '@codemirror/autocomplete'
import { linter, lintGutter, type Diagnostic } from '@codemirror/lint'
import { bracketMatching, foldGutter, indentOnInput, syntaxHighlighting, defaultHighlightStyle } from '@codemirror/language'
import { pipelinesApi, type YamlValidationResult } from '@/api/pipelines'
import { createPipelineYamlCompletionSource } from '@/utils/pipelineYamlCompletions'
import { diagnosticsFromValidation } from '@/utils/pipelineYamlLint'

const props = withDefaults(
  defineProps<{
    modelValue: string
    secretNames?: string[]
    variableNames?: string[]
    placeholder?: string
    /** Debounce for remote validate lint (ms). */
    lintDebounceMs?: number
  }>(),
  {
    secretNames: () => [],
    variableNames: () => [],
    placeholder: 'name: hello-world\nsteps:\n  - name: greet\n    run: echo Hello',
    lintDebounceMs: 550,
  },
)

const emit = defineEmits<{
  'update:modelValue': [value: string]
  validated: [result: YamlValidationResult | null]
}>()

const hostRef = ref<HTMLDivElement | null>(null)
let view: EditorView | null = null

const completionCompartment = new Compartment()
const lintCompartment = new Compartment()

let lintTimer: ReturnType<typeof setTimeout> | null = null
let lintSeq = 0
let lastDiagnostics: Diagnostic[] = []
let lastResult: YamlValidationResult | null = null

const editorTheme = EditorView.theme({
  '&': {
    height: '100%',
    fontSize: '13px',
    backgroundColor: 'transparent',
  },
  '.cm-scroller': {
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
    lineHeight: '1.7',
    overflow: 'auto',
  },
  '.cm-content': {
    padding: '10px 0',
    caretColor: 'hsl(var(--foreground))',
  },
  '.cm-gutters': {
    backgroundColor: 'hsl(var(--muted) / 0.45)',
    color: 'hsl(var(--muted-foreground) / 0.7)',
    border: 'none',
    borderRight: '1px solid hsl(var(--border) / 0.5)',
  },
  '.cm-activeLineGutter': {
    backgroundColor: 'hsl(var(--muted) / 0.7)',
  },
  '.cm-activeLine': {
    backgroundColor: 'hsl(var(--muted) / 0.35)',
  },
  '&.cm-focused': {
    outline: 'none',
  },
  '.cm-selectionBackground, &.cm-focused .cm-selectionBackground': {
    backgroundColor: 'hsl(var(--primary) / 0.18) !important',
  },
  '.cm-cursor': {
    borderLeftColor: 'hsl(var(--foreground))',
  },
  '.cm-tooltip': {
    backgroundColor: 'hsl(var(--popover))',
    color: 'hsl(var(--popover-foreground))',
    border: '1px solid hsl(var(--border))',
    borderRadius: '8px',
    boxShadow: '0 8px 24px rgb(0 0 0 / 0.12)',
  },
  '.cm-tooltip.cm-tooltip-autocomplete > ul > li[aria-selected]': {
    backgroundColor: 'hsl(var(--accent))',
    color: 'hsl(var(--accent-foreground))',
  },
  '.cm-diagnostic-error': {
    borderLeftColor: 'hsl(var(--destructive))',
  },
  /* Stronger underline + wash on the exact problem token (bad key, etc.) */
  '.cm-buz-lint-error': {
    backgroundColor: 'hsl(var(--destructive) / 0.14)',
    borderBottom: '2px solid hsl(var(--destructive))',
    borderRadius: '2px',
  },
  '.cm-buz-lint-warning': {
    backgroundColor: 'hsl(38 92% 50% / 0.12)',
    borderBottom: '2px solid hsl(38 92% 40%)',
    borderRadius: '2px',
  },
})

function completionExt() {
  return autocompletion({
    override: [
      createPipelineYamlCompletionSource({
        secretNames: props.secretNames,
        variableNames: props.variableNames,
      }),
    ],
    activateOnTyping: true,
  })
}

function scheduleLint(doc: string) {
  if (lintTimer) clearTimeout(lintTimer)
  lintTimer = setTimeout(() => {
    void runLint(doc)
  }, props.lintDebounceMs)
}

async function runLint(doc: string) {
  const seq = ++lintSeq
  if (!doc.trim()) {
    lastDiagnostics = []
    lastResult = null
    emit('validated', null)
    view?.dispatch({ effects: lintCompartment.reconfigure(lintExt()) })
    return
  }
  try {
    const result = await pipelinesApi.validateYaml(doc)
    if (seq !== lintSeq) return
    lastResult = result
    lastDiagnostics = diagnosticsFromValidation(doc, result)
    emit('validated', result)
    view?.dispatch({ effects: lintCompartment.reconfigure(lintExt()) })
  } catch {
    if (seq !== lintSeq) return
    lastDiagnostics = [{
      from: 0,
      to: Math.min(doc.length, 1),
      severity: 'error',
      message: 'Validation request failed',
    }]
    lastResult = {
      valid: false,
      name: null,
      errors: [{ field: 'network', message: 'Validation request failed' }],
      warnings: [],
    }
    emit('validated', lastResult)
    view?.dispatch({ effects: lintCompartment.reconfigure(lintExt()) })
  }
}

function lintExt() {
  return [
    lintGutter(),
    linter(
      () => lastDiagnostics,
      { delay: 0 },
    ),
  ]
}

function buildExtensions() {
  return [
    lineNumbers(),
    highlightActiveLine(),
    highlightActiveLineGutter(),
    foldGutter(),
    drawSelection(),
    indentOnInput(),
    bracketMatching(),
    closeBrackets(),
    history(),
    yaml(),
    syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
    editorTheme,
    cmPlaceholder(props.placeholder),
    keymap.of([
      ...closeBracketsKeymap,
      ...defaultKeymap,
      ...historyKeymap,
      ...completionKeymap,
      indentWithTab,
    ]),
    completionCompartment.of(completionExt()),
    lintCompartment.of(lintExt()),
    EditorView.updateListener.of((update) => {
      if (update.docChanged) {
        const value = update.state.doc.toString()
        emit('update:modelValue', value)
        scheduleLint(value)
      }
    }),
    EditorView.lineWrapping,
  ]
}

onMounted(() => {
  if (!hostRef.value) return
  const state = EditorState.create({
    doc: props.modelValue || '',
    extensions: buildExtensions(),
  })
  view = new EditorView({
    state,
    parent: hostRef.value,
  })
  if (props.modelValue.trim()) scheduleLint(props.modelValue)
})

onBeforeUnmount(() => {
  if (lintTimer) clearTimeout(lintTimer)
  view?.destroy()
  view = null
})

watch(
  () => props.modelValue,
  (next) => {
    if (!view) return
    const cur = view.state.doc.toString()
    if (next === cur) return
    view.dispatch({
      changes: { from: 0, to: cur.length, insert: next || '' },
    })
  },
)

watch(
  () => [props.secretNames, props.variableNames] as const,
  () => {
    view?.dispatch({
      effects: completionCompartment.reconfigure(completionExt()),
    })
  },
  { deep: true },
)

/** Force an immediate validate (e.g. before Save). */
async function validateNow(): Promise<YamlValidationResult | null> {
  if (lintTimer) clearTimeout(lintTimer)
  const doc = view?.state.doc.toString() ?? props.modelValue
  await runLint(doc)
  return lastResult
}

function focus() {
  view?.focus()
}

defineExpose({ validateNow, focus })
</script>

<template>
  <div
    ref="hostRef"
    class="pipeline-yaml-cm h-full min-h-0 w-full overflow-hidden bg-transparent text-sm"
  />
</template>

<style>
.pipeline-yaml-cm .cm-editor {
  height: 100%;
}
.pipeline-yaml-cm .cm-editor.cm-focused {
  outline: none;
}
</style>
