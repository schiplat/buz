<script setup lang="ts">
import { ref, onMounted, onUnmounted, watch, nextTick } from 'vue'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'
import type { LogChunk } from '@/api/jobs'
import { formatLogClock } from '@/utils/time'
import { formatLogChunkForTerminal } from '@/utils/logFormat'

const props = defineProps<{
  logs: LogChunk[]
  stepOrder: number
}>()

const containerEl = ref<HTMLDivElement | null>(null)
let term: Terminal | null = null
let fitAddon: FitAddon | null = null
/** Number of chunks already written to the terminal — lets live polls append
 *  only new chunks instead of clearing and re-writing (which would flicker). */
let writtenCount = 0

const terminalTheme = {
  background: '#0d1117',
  foreground: '#c9d1d9',
  cursor: '#c9d1d9',
  selectionBackground: '#264f78',
  black: '#484f58', red: '#ff7b72', green: '#3fb950', yellow: '#d29922',
  blue: '#58a6ff', magenta: '#bc8cff', cyan: '#39c5cf', white: '#b1bac4',
  brightBlack: '#6e7681', brightRed: '#ffa198', brightGreen: '#56d364',
  brightYellow: '#e3b341', brightBlue: '#79c0ff', brightMagenta: '#d2a8ff',
  brightCyan: '#56d4dd', brightWhite: '#f0f6fc',
}

function clockFor(chunk: LogChunk): string {
  return formatLogClock(chunk.created_at, '--:--:--')
}

/** Write any chunks not yet rendered (incremental; avoids full clear+flicker). */
function syncLogs() {
  if (!term) return
  const filtered = props.logs.filter(c => c.step_order === props.stepOrder)
  for (let i = writtenCount; i < filtered.length; i++) {
    const chunk = filtered[i]
    const ts = clockFor(chunk)
    term.write(formatLogChunkForTerminal(ts, chunk))
  }
  writtenCount = filtered.length
}

function initTerminal() {
  if (!containerEl.value || term) return
  fitAddon = new FitAddon()
  term = new Terminal({
    cursorBlink: false, disableStdin: true, fontSize: 11,
    fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', monospace",
    theme: terminalTheme,
    allowProposedApi: true,
  })
  term.loadAddon(fitAddon)
  term.open(containerEl.value)
  fitAddon.fit()

  // Write initial logs
  writtenCount = 0
  syncLogs()
}

onMounted(async () => {
  await nextTick()
  initTerminal()
})

watch(() => props.logs, () => {
  syncLogs()
}, { deep: true })

onUnmounted(() => {
  term?.dispose()
  term = null
})

defineExpose({ fit: () => fitAddon?.fit() })
</script>

<template>
  <div ref="containerEl" class="h-full w-full" />
</template>
