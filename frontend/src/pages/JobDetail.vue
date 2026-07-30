<script setup lang="ts">
import { ref, onMounted, onUnmounted, nextTick, computed, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import { ArrowLeft, RefreshCw, ChevronDown, ChevronRight, XCircle, Trash2, CheckCircle2, Loader2, MinusCircle } from '@lucide/vue'
import { jobsApi, type Job, type LogChunk } from '@/api/jobs'
import { parseUtc, formatLogClock } from '@/utils/time'
import { createManagedWebSocket, type ManagedWebSocket } from '@/utils/ws'
import { navLog } from '@/utils/navigation'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'

const route = useRoute()
const router = useRouter()
const jobId = route.params.id as string

const job = ref<Job | null>(null)
const logs = ref<LogChunk[]>([])
const loading = ref(true)
let refreshTimer: ReturnType<typeof setInterval> | null = null
let pollInFlight = false
let pollAbort: AbortController | null = null
let alive = true

// ── Per-step terminals ──
interface StepTerminal {
  stepOrder: number
  term: Terminal
  fitAddon: FitAddon
  seenContent: Set<string>
}

const stepTerminals = ref<StepTerminal[]>([])
const expandedSteps = ref<Set<number>>(new Set())
let managedWs: ManagedWebSocket | null = null
const targetStep = ref<number | null>(null)

/** xterm container grows with log lines (capped so extreme jobs still scroll inside). */
const TERM_FONT_SIZE = 12
const TERM_LINE_HEIGHT = 18
const TERM_MIN_ROWS = 3
const TERM_MAX_ROWS = 40

function countStepLogLines(stepOrder: number): number {
  const chunks = stepLogs.value.get(stepOrder) || []
  let lines = 0
  for (const c of chunks) {
    // Normalize; a trailing newline is a terminator, not an extra blank row
    let text = c.content.replace(/\r\n/g, '\n').replace(/\r/g, '\n')
    if (text.endsWith('\n')) text = text.slice(0, -1)
    if (text.length === 0) continue
    lines += text.split('\n').length
  }
  return lines
}

function fitTerminalHeight(stepOrder: number) {
  const el = document.getElementById('terminal-el-' + stepOrder) as HTMLDivElement | null
  if (!el) return
  const lines = countStepLogLines(stepOrder)
  const rows = lines === 0
    ? 4 // room for empty-state message
    : Math.min(TERM_MAX_ROWS, Math.max(TERM_MIN_ROWS, lines))
  // Height = text rows only; equal inset comes from the wrapper's py-* (xterm ignores parent padding)
  el.style.padding = '0'
  el.style.height = `${rows * TERM_LINE_HEIGHT}px`
  const st = stepTerminals.value.find(t => t.stepOrder === stepOrder)
  if (st) {
    requestAnimationFrame(() => {
      try { st.fitAddon.fit() } catch { /* disposed */ }
    })
  }
}

const stepLogs = computed(() => {
  const groups: Map<number, LogChunk[]> = new Map()
  for (const chunk of logs.value) {
    const so = chunk.step_order
    if (!groups.has(so)) groups.set(so, [])
    groups.get(so)!.push(chunk)
  }
  return groups
})

function getStepStatus(stepOrder: number): string {
  if (!job.value?.step_statuses) return 'pending'
  const found = job.value.step_statuses.find((s: any) => Number(s.step_order) === stepOrder)
  return found?.status || 'pending'
}

function fmtStepDuration(stepOrder: number): string {
  if (!job.value?.step_statuses) return '\u2014'
  const found = job.value.step_statuses.find((s: any) => Number(s.step_order) === stepOrder)
  if (!found?.started_at) return '\u2014'
  const a = parseUtc(found.started_at)
  const b = found.finished_at ? parseUtc(found.finished_at) : Date.now()
  const ms = Math.max(0, b - a)
  return (ms / 1000).toFixed(3) + 's'
}

function nowTimeStr(): string {
  const n = new Date()
  return `${String(n.getHours()).padStart(2, '0')}:${String(n.getMinutes()).padStart(2, '0')}:${String(n.getSeconds()).padStart(2, '0')}`
}

function findActiveStep(stepStatuses: any[] | null | undefined): number | null {
  if (!stepStatuses) return null
  const found = stepStatuses.find((s: any) => s.status === 'running')
  return found != null ? Number(found.step_order) : null
}

function getCompletedSteps(stepStatuses: any[] | null | undefined): Set<number> {
  if (!stepStatuses) return new Set()
  return new Set(
    stepStatuses
      .filter((s: any) => s.status === 'success' || s.status === 'failed' || s.status === 'cancelled')
      .map((s: any) => Number(s.step_order))
  )
}

onMounted(async () => {
  await loadJob()
  if (route.query.step != null) {
    targetStep.value = Number(route.query.step)
  } else {
    // Auto-expand the active-running step during live build
    const activeIdx = findActiveStep(job.value?.step_statuses)
    if (activeIdx != null) {
      targetStep.value = activeIdx
      router.replace({ query: { step: activeIdx } })
    }
  }
  await nextTick()
  if (job.value?.snapshot?.tasks) {
    if (targetStep.value != null) expandedSteps.value.add(targetStep.value)
    await nextTick()
    if (targetStep.value != null) initSingleTerminal(targetStep.value)
  }
  startRefreshTimer()
  startWebSocket()
  if (targetStep.value != null) {
    await nextTick()
    document.getElementById('step-' + targetStep.value)?.scrollIntoView({ behavior: 'smooth', block: 'center' })
  }
})

onUnmounted(() => {
  alive = false
  clearRefreshTimer()
  managedWs?.close()
  managedWs = null
  for (const st of stepTerminals.value) st.term.dispose()
  stepTerminals.value = []
})

async function loadJob() {
  loading.value = true
  try {
    const [j, l] = await Promise.all([jobsApi.get(jobId), jobsApi.logs(jobId)])
    job.value = j
    logs.value = l
  } catch (e) { console.error(e) }
  finally { loading.value = false }
}

function startRefreshTimer() {
  if (refreshTimer) clearInterval(refreshTimer)
  // Lightweight log-only fallback — status updates come via WebSocket.
  // Skip if a poll is already in flight so hung requests cannot pile up and
  // exhaust the browser per-host connection pool (which freezes SPA nav).
  refreshTimer = setInterval(async () => {
    const s = job.value?.status
    if (s !== 'running' && s !== 'pending') { clearRefreshTimer(); return }
    if (pollInFlight || !alive) return
    pollInFlight = true
    pollAbort?.abort()
    pollAbort = new AbortController()
    try {
      const l = await jobsApi.logs(jobId, { signal: pollAbort.signal, timeout: 5_000 })
      if (!alive) return
      const oldLen = logs.value.length
      if (l.length > oldLen) {
        logs.value = l
        const activeIdx = findActiveStep(job.value?.step_statuses)
        const st = stepTerminals.value.find(t => t.stepOrder === activeIdx)
        if (st && l.length > oldLen) {
          for (let i = oldLen; i < l.length; i++) {
            const c = l[i]
            if (c.step_order === activeIdx && !st.seenContent.has(c.content)) {
              st.seenContent.add(c.content)
              const ts = formatLogClock(c.created_at, nowTimeStr())
              st.term.write(`\x1b[2m[${ts}]\x1b[0m ${c.content.replace(/\n/g, '\r\n')}`)
            }
          }
          fitTerminalHeight(activeIdx!)
        }
      }
    } catch {
      /* silent — network blips during manager restart */
    } finally {
      pollInFlight = false
    }
  }, 3000)
}

function clearRefreshTimer() {
  if (refreshTimer) { clearInterval(refreshTimer); refreshTimer = null }
  pollAbort?.abort()
  pollAbort = null
  pollInFlight = false
}

function initSingleTerminal(stepOrder: number) {
  // Skip if already created
  if (stepTerminals.value.find(t => t.stepOrder === stepOrder)) return

  const el = document.getElementById('terminal-el-' + stepOrder) as HTMLDivElement | null
  if (!el) return

  fitTerminalHeight(stepOrder)

  const fitAddon = new FitAddon()
  const term = new Terminal({
    cursorBlink: false, disableStdin: true, fontSize: TERM_FONT_SIZE,
    fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', monospace",
    theme: {
      background: '#0d1117', foreground: '#c9d1d9',
      cursor: '#c9d1d9', selectionBackground: '#264f78',
      black: '#484f58', red: '#ff7b72', green: '#3fb950', yellow: '#d29922',
      blue: '#58a6ff', magenta: '#bc8cff', cyan: '#39c5cf', white: '#b1bac4',
      brightBlack: '#6e7681', brightRed: '#ffa198', brightGreen: '#56d364',
      brightYellow: '#e3b341', brightBlue: '#79c0ff', brightMagenta: '#d2a8ff',
      brightCyan: '#56d4dd', brightWhite: '#f0f6fc',
    },
    allowProposedApi: true,
    scrollback: 5000,
  })
  term.loadAddon(fitAddon)
  term.open(el)
  fitAddon.fit()

  const chunks = stepLogs.value.get(stepOrder) || []
  const hasEcho = chunks.some(c => c.content.startsWith('==> RUN'))

  const seen = new Set<string>()

  // Fallback: synthesize echo header from snapshot if the DB echo chunk is missing
  if (!hasEcho && job.value?.snapshot) {
    const task = job.value.snapshot.tasks.find(t => Number(t.step_order) === stepOrder)
    if (task) {
      const parts = [`==> RUN [${task.exec_type}]`]
      parts.push('note: GIT_TERMINAL_PROMPT=0 (noninteractive) — interactive prompts will cause failure')
      if (task.work_dir) parts.push(`cwd: ${task.work_dir}`)
      const envVars = job.value.snapshot.env || []
      if (envVars.length > 0) {
        const items = envVars.map(e => e.is_secret ? `${e.name}=***` : `${e.name}`)
        parts.push(`env: [${items.join(', ')}]`)
      }
      parts.push(`cmd: ${task.command_template}`)
      const fallbackEcho = parts.join('\n') + '\n'
      const ts = '--:--:--'
      term.write(`\x1b[2m[${ts}]\x1b[0m ${fallbackEcho.replace(/\n/g, '\r\n')}`)
      seen.add(fallbackEcho)
    }
  }

  for (const chunk of chunks) {
    const ts = formatLogClock(chunk.created_at, nowTimeStr())
    term.write(`\x1b[2m[${ts}]\x1b[0m ${chunk.content.replace(/\n/g, '\r\n')}`)
    seen.add(chunk.content)
  }
  stepTerminals.value.push({ stepOrder, term, fitAddon, seenContent: seen })
  fitTerminalHeight(stepOrder)
}

function handleWsMessage(raw: string) {
  if (!alive) return
  try {
    const data = JSON.parse(raw)
    // ── Status update from backend ──
    if (data.type === 'status' && data.data) {
      const newStatus = data.data
      if (job.value) {
        job.value = {
          ...job.value,
          status: newStatus.status,
          step_statuses: newStatus.step_statuses || job.value.step_statuses,
          start_time: newStatus.start_time ?? job.value.start_time,
          end_time: newStatus.end_time ?? job.value.end_time,
        }
        // Stop the fallback timer when job reaches a terminal state
        if (newStatus.status !== 'running' && newStatus.status !== 'pending') {
          clearRefreshTimer()
          // Prefer DB timestamps (created_at) over live WS chunks that lacked them
          void jobsApi.logs(jobId).then((l) => {
            if (alive) logs.value = l
          }).catch(() => { /* ignore */ })
        }
        // Auto-expand / auto-collapse steps
        const activeIdx = findActiveStep(job.value.step_statuses)
        const completed = getCompletedSteps(job.value.step_statuses)
        for (const so of expandedSteps.value) {
          if (completed.has(so) && so !== activeIdx) {
            expandedSteps.value.delete(so)
            const idx = stepTerminals.value.findIndex(t => t.stepOrder === so)
            if (idx !== -1) {
              stepTerminals.value[idx].term.dispose()
              stepTerminals.value.splice(idx, 1)
            }
          }
        }
        if (activeIdx != null && !expandedSteps.value.has(activeIdx)) {
          expandedSteps.value.add(activeIdx)
          targetStep.value = activeIdx
          router.replace({ query: { step: activeIdx } })
          nextTick(() => {
            if (!alive) return
            initSingleTerminal(activeIdx)
            fitTerminalHeight(activeIdx)
          })
        }
      }
      return
    }
    // ── Log chunk (existing format) ──
    const chunk = data as LogChunk
    if (!chunk.created_at) {
      chunk.created_at = new Date().toISOString()
    }
    // Avoid duplicating chunks already loaded via REST
    if (logs.value.some(c => c.step_order === chunk.step_order && c.content === chunk.content)) {
      return
    }
    logs.value.push(chunk)
    const st = stepTerminals.value.find(t => t.stepOrder === chunk.step_order)
    if (st && !st.seenContent.has(chunk.content)) {
      st.seenContent.add(chunk.content)
      const ts = formatLogClock(chunk.created_at, nowTimeStr())
      st.term.write(`\x1b[2m[${ts}]\x1b[0m ${chunk.content.replace(/\n/g, '\r\n')}`)
      fitTerminalHeight(chunk.step_order)
    }
  } catch {
    // Old plaintext fallback
    logs.value.push({ step_order: -1, content: raw } as LogChunk)
    const running = stepTerminals.value.find(t => getStepStatus(t.stepOrder) === 'running')
    if (running && !running.seenContent.has(raw)) {
      running.seenContent.add(raw)
      running.term.write(`\x1b[2m[${nowTimeStr()}]\x1b[0m ${raw.replace(/\n/g, '\r\n')}`)
      fitTerminalHeight(running.stepOrder)
    }
  }
}

function startWebSocket() {
  managedWs?.close()
  const token = localStorage.getItem('buz_token') || ''
  const protocol = window.location.protocol === 'https:' ? 'wss' : 'ws'
  const wsUrl = `${protocol}://${window.location.host}/api/v1/jobs/${jobId}/logs/stream?token=${encodeURIComponent(token)}`
  managedWs = createManagedWebSocket({
    label: `job:${jobId.slice(0, 8)}`,
    onMessage: handleWsMessage,
    onOpen: () => navLog('[JobDetail] WS connected'),
  })
  managedWs.connect(wsUrl)
}

function toggleStep(stepOrder: number) {
  // Prevent expanding pending steps (no logs to show yet)
  if (getStepStatus(stepOrder) === 'pending') return
  // Update the active highlight
  targetStep.value = stepOrder
  // Update URL without full navigation
  router.replace({ query: { step: stepOrder } })
  if (expandedSteps.value.has(stepOrder)) {
    expandedSteps.value.delete(stepOrder)
    // Dispose the terminal so it gets recreated when re-expanded
    const idx = stepTerminals.value.findIndex(t => t.stepOrder === stepOrder)
    if (idx !== -1) {
      stepTerminals.value[idx].term.dispose()
      stepTerminals.value.splice(idx, 1)
    }
  } else {
    expandedSteps.value.add(stepOrder)
    nextTick(() => {
      initSingleTerminal(stepOrder)
      fitTerminalHeight(stepOrder)
    })
  }
}

// Watch for external URL changes (browser back/forward, or from Pipeline Detail)
watch(() => route.query.step, (step) => {
  const n = step != null ? Number(step) : null
  if (n != null && n !== targetStep.value) {
    targetStep.value = n
    expandedSteps.value.add(n)
    nextTick(() => {
      document.getElementById('step-' + n)?.scrollIntoView({ behavior: 'smooth', block: 'center' })
      initSingleTerminal(n)
      fitTerminalHeight(n)
    })
  }
})

async function handleCancel() {
  try { clearRefreshTimer(); await jobsApi.cancel(jobId); await loadJob() } catch (e) { console.error(e) }
}

async function handleDelete() {
  if (!window.confirm('Permanently delete this job and all its logs?')) return
  try { clearRefreshTimer(); managedWs?.close(); await jobsApi.delete(jobId); router.push(`/pipelines/${job.value?.pipeline_id}/history`) } catch (e) { console.error(e) }
}

function execLabel(t: string): string {
  return { local: 'Runner', ssh: 'SSH', docker: 'Docker', wasm: 'Wasm' }[t] ?? t
}

function stepStatusColor(status: string): string {
  switch (status) {
    case 'success': return 'border-emerald-400/40 bg-emerald-500/[0.04] text-emerald-500'
    case 'failed': return 'border-red-400/40 bg-red-500/[0.04] text-red-500'
    case 'running': return 'border-amber-400/40 bg-amber-500/[0.04] text-amber-500'
    case 'cancelled': return 'border-slate-400/30 bg-slate-500/[0.03] text-slate-400'
    default: return 'border-border/50 bg-muted/30 text-muted-foreground'
  }
}

function terminalTitleStatus(status: string): { dot: string; label: string } {
  switch (status) {
    case 'running': return { dot: 'bg-amber-400 shadow-[0_0_6px_rgba(251,191,36,0.5)]', label: 'Running' }
    case 'success': return { dot: 'bg-emerald-400', label: 'Success' }
    case 'failed': return { dot: 'bg-red-400', label: 'Failed' }
    case 'cancelled': return { dot: 'bg-slate-400', label: 'Cancelled' }
    default: return { dot: 'bg-slate-500', label: 'Idle' }
  }
}
</script>

<template>
  <div class="space-y-5">
    <!-- Top bar -->
    <div class="flex items-center gap-3">
      <button class="inline-flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground transition-colors" @click="router.push('/pipelines/' + (job?.pipeline_id || '') + '/history')">
        <ArrowLeft class="h-4 w-4" />
      </button>
      <PageHeader :title="`Job ${jobId}`" :description="job?.pipeline_name || 'Loading...'">
        <template #actions>
          <UiButton v-if="job?.status === 'running' || job?.status === 'pending'" variant="outline" size="sm" class="border-red-500/30 text-red-600 hover:bg-red-500/10 text-xs px-3 py-1.5" @click="handleCancel">
            <XCircle class="h-3.5 w-3.5" /> Cancel
          </UiButton>
          <UiButton variant="outline" size="sm" class="border-red-500/20 text-red-500 hover:bg-red-500/10 text-xs px-3 py-1.5" @click="handleDelete">
            <Trash2 class="h-3.5 w-3.5" /> Delete
          </UiButton>
          <UiButton variant="outline" size="sm" class="text-xs px-3 py-1.5" @click="loadJob" :disabled="loading">
            <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': loading }" /> Refresh
          </UiButton>
        </template>
      </PageHeader>
    </div>

    <div v-if="loading && !job" class="flex items-center justify-center py-16">
      <div class="space-y-3 text-center">
        <Loader2 class="h-5 w-5 mx-auto text-muted-foreground animate-spin" />
        <p class="text-sm text-muted-foreground">Loading job details...</p>
      </div>
    </div>

    <template v-else-if="job">
      <!-- Job Info -->
      <div class="rounded-2xl border border-border/50 bg-card/60 backdrop-blur-sm p-6">
        <h3 class="text-xs font-semibold uppercase tracking-[0.15em] text-muted-foreground/70 mb-5">Job Info</h3>
        <div class="grid grid-cols-2 sm:grid-cols-4 gap-x-8 gap-y-4">
          <div>
            <div class="text-[10px] uppercase tracking-[0.1em] text-muted-foreground/50 mb-1.5 font-medium">Status</div>
            <StatusBadge :status="job.status" />
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-[0.1em] text-muted-foreground/50 mb-1.5 font-medium">Pipeline</div>
            <div class="text-sm font-medium text-foreground/90">{{ job.pipeline_name || job.pipeline_id }}</div>
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-[0.1em] text-muted-foreground/50 mb-1.5 font-medium">Runner</div>
            <div class="font-mono text-xs text-muted-foreground">{{ job.runner_id ? String(job.runner_id).slice(0, 8) + '...' : '\u2014' }}</div>
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-[0.1em] text-muted-foreground/50 mb-1.5 font-medium">Trigger</div>
            <div class="font-mono text-xs text-muted-foreground">{{ job.trigger_by }}</div>
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-[0.1em] text-muted-foreground/50 mb-1.5 font-medium">Retry</div>
            <div class="font-mono text-sm tabular-nums text-foreground/80">{{ job.retry_times }}</div>
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-[0.1em] text-muted-foreground/50 mb-1.5 font-medium">Started</div>
            <div class="text-xs text-muted-foreground">{{ job.start_time || '\u2014' }}</div>
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-[0.1em] text-muted-foreground/50 mb-1.5 font-medium">Ended</div>
            <div class="text-xs text-muted-foreground">{{ job.end_time || '\u2014' }}</div>
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-[0.1em] text-muted-foreground/50 mb-1.5 font-medium">Created</div>
            <div class="text-xs text-muted-foreground">{{ job.created_at }}</div>
          </div>
        </div>
      </div>

      <!-- Pipeline Snapshot + Steps with embedded terminals -->
      <div v-if="job.snapshot && job.snapshot.tasks && job.snapshot.tasks.length > 0" class="rounded-2xl border border-border/50 bg-card/60 backdrop-blur-sm">
        <!-- Section header -->
        <div class="flex items-center justify-between px-6 py-4 border-b border-border/30">
          <div class="flex items-center gap-3">
            <h3 class="text-xs font-semibold uppercase tracking-[0.15em] text-muted-foreground/70">Steps</h3>
            <span class="text-[10px] font-mono tabular-nums bg-muted rounded-md px-2 py-0.5 text-muted-foreground">{{ job.snapshot.tasks.length }}</span>
            <span class="text-[10px] font-mono bg-slate-100 dark:bg-slate-800 rounded-md px-2 py-0.5 text-muted-foreground">Snapshot</span>
            <span v-if="job.snapshot.triggered_at" class="text-[11px] text-muted-foreground/60 font-mono">{{ job.snapshot.triggered_at.slice(0, 19).replace('T', ' ') }}</span>
          </div>
          <router-link
            :to="'/pipelines/' + job.pipeline_id + '/definition'"
            class="text-[11px] text-muted-foreground/60 hover:text-foreground transition-colors underline underline-offset-2"
          >View pipeline definition</router-link>
        </div>

        <div class="px-6 py-5 space-y-4">
          <!-- Runner Labels -->
          <div v-if="job.snapshot.runner_labels && job.snapshot.runner_labels.length > 0" class="flex items-center gap-2">
            <span class="text-[10px] uppercase tracking-[0.1em] text-muted-foreground/50 font-medium">Runner</span>
            <div class="flex flex-wrap gap-1.5">
              <span v-for="l in job.snapshot.runner_labels" :key="l" class="text-[10px] font-mono bg-muted rounded-md px-2 py-0.5 text-muted-foreground">{{ l }}</span>
            </div>
          </div>

          <!-- Step cards with inline terminals -->
          <div class="space-y-1.5">
            <div v-for="t in job.snapshot.tasks" :key="t.step_order" :id="'step-' + t.step_order">
              <!-- Step row -->
              <div
                class="group flex items-center gap-3 rounded-xl border px-4 py-3 transition-all duration-150"
                :class="[
                  getStepStatus(t.step_order) === 'pending'
                    ? 'border-border/40 bg-muted/20 opacity-50 cursor-not-allowed'
                    : 'cursor-pointer',
                  targetStep === t.step_order
                    ? 'border-primary/30 bg-primary/[0.03] shadow-sm'
                    : (getStepStatus(t.step_order) === 'pending'
                      ? ''
                      : 'border-border/40 bg-transparent hover:bg-muted/40 hover:border-border')
                ]"
                @click="getStepStatus(t.step_order) !== 'pending' && toggleStep(t.step_order)"
              >
                <!-- Step status icon -->
                <div class="shrink-0">
                  <CheckCircle2 v-if="getStepStatus(t.step_order) === 'success'" class="h-4.5 w-4.5 text-emerald-500" />
                  <XCircle v-else-if="getStepStatus(t.step_order) === 'failed'" class="h-4.5 w-4.5 text-red-500" />
                  <Loader2 v-else-if="getStepStatus(t.step_order) === 'running'" class="h-4.5 w-4.5 text-amber-400 animate-spin" />
                  <MinusCircle v-else-if="getStepStatus(t.step_order) === 'cancelled'" class="h-4.5 w-4.5 text-slate-400" />
                  <span v-else class="inline-flex items-center justify-center h-[18px] w-[18px] rounded-full bg-muted text-[10px] font-bold text-muted-foreground/60 tabular-nums leading-none">{{ t.step_order + 1 }}</span>
                </div>

                <!-- Step main content -->
                <div class="flex-1 min-w-0">
                  <div class="flex items-center gap-2 flex-wrap">
                    <span class="text-[13px] font-semibold text-foreground/90 truncate max-w-[280px]">{{ t.name }}</span>
                    <span class="text-[9px] font-mono uppercase tracking-wider rounded-md border border-border/40 px-1.5 py-0 text-muted-foreground/60">{{ execLabel(t.exec_type) }}</span>
                    <span v-if="t.host_id" class="text-[9px] font-mono rounded-md bg-muted px-1.5 py-0 text-muted-foreground/60 truncate max-w-[120px]">{{ t.host_id }}</span>
                  </div>
                  <div class="font-mono text-[11px] text-muted-foreground/60 whitespace-pre-wrap break-all leading-relaxed mt-1">{{ t.command_template }}</div>
                  <div v-if="t.fail_strategy === 'continue'" class="mt-1.5">
                    <span class="inline-flex items-center gap-1 text-[10px] text-amber-600 bg-amber-500/[0.06] border border-amber-500/20 rounded-md px-2 py-0 font-medium">continue-on-error</span>
                  </div>
                </div>

                <!-- Right: duration + toggle -->
                <div class="flex items-center gap-3 shrink-0">
                  <span
                    class="text-[11px] font-mono tabular-nums"
                    :class="getStepStatus(t.step_order) === 'running'
                      ? 'text-amber-500 font-medium'
                      : getStepStatus(t.step_order) === 'pending'
                        ? 'text-muted-foreground/30'
                        : 'text-muted-foreground/50'"
                  >{{ fmtStepDuration(t.step_order) }}</span>
                  <template v-if="getStepStatus(t.step_order) !== 'pending'">
                    <div class="h-5 w-5 flex items-center justify-center rounded-md text-muted-foreground/40 group-hover:text-muted-foreground transition-colors">
                      <ChevronDown v-if="expandedSteps.has(t.step_order)" class="h-3.5 w-3.5" />
                      <ChevronRight v-else class="h-3.5 w-3.5" />
                    </div>
                  </template>
                </div>
              </div>

              <!-- Step log panel -->
              <div
                v-if="expandedSteps.has(t.step_order)"
                class="mt-1.5 overflow-hidden rounded-xl border border-border bg-muted/30"
              >
                <div class="flex items-center gap-2 h-8 px-3 border-b border-border/60 bg-muted/40 select-none">
                  <span class="text-[11px] font-medium text-muted-foreground truncate">{{ t.name }}</span>
                  <span class="text-[9px] font-mono uppercase rounded px-1.5 py-0 border border-border/50 text-muted-foreground/70 leading-normal">{{ execLabel(t.exec_type) }}</span>
                  <div class="flex-1" />
                  <div class="flex items-center gap-1.5 shrink-0">
                    <span class="inline-block w-1.5 h-1.5 rounded-full" :class="terminalTitleStatus(getStepStatus(t.step_order)).dot" />
                    <span class="text-[10px] text-muted-foreground/70 font-medium">{{ terminalTitleStatus(getStepStatus(t.step_order)).label }}</span>
                  </div>
                </div>
                <div class="relative w-full bg-[#0d1117] px-3 py-3">
                  <div :id="'terminal-el-' + t.step_order" class="w-full" />
                  <div
                    v-if="(stepLogs.get(t.step_order) || []).length === 0"
                    class="absolute inset-0 flex items-center justify-center pointer-events-none"
                  >
                    <div class="text-center space-y-1.5">
                      <Loader2 v-if="job.status === 'running'" class="h-3.5 w-3.5 mx-auto text-muted-foreground/50 animate-spin" />
                      <MinusCircle v-else class="h-3.5 w-3.5 mx-auto text-muted-foreground/40" />
                      <p class="text-[11px] text-muted-foreground/50">{{ job.status === 'running' ? 'Waiting for output...' : 'No output recorded' }}</p>
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </div>

          <!-- Trigger Inputs -->
          <div v-if="job.snapshot.inputs && Object.keys(job.snapshot.inputs).length > 0" class="pt-1 border-t border-border/20">
            <span class="text-[10px] uppercase tracking-[0.1em] text-muted-foreground/50 font-medium">Trigger Inputs</span>
            <div class="flex flex-wrap gap-1.5 mt-2">
              <span
                v-for="(val, key) in job.snapshot.inputs" :key="key"
                class="inline-flex items-center gap-1.5 rounded-md border border-primary/15 bg-primary/[0.03] px-2.5 py-1 text-[11px] font-mono"
              >
                <span class="text-muted-foreground/70">{{ key }}</span>
                <span class="text-foreground/70 font-medium">{{ val }}</span>
              </span>
            </div>
          </div>

          <!-- Env & Secrets -->
          <div v-if="job.snapshot.env && job.snapshot.env.length > 0" class="pt-1 border-t border-border/20">
            <span class="text-[10px] uppercase tracking-[0.1em] text-muted-foreground/50 font-medium">Environment &amp; Secrets</span>
            <div class="flex flex-wrap gap-1.5 mt-2">
              <span
                v-for="e in job.snapshot.env" :key="e.name"
                class="inline-flex items-center gap-1 rounded-md border px-2 py-1 text-[11px] font-mono"
                :class="e.is_secret
                  ? 'border-amber-400/15 bg-amber-400/[0.03] text-amber-500/80'
                  : 'border-border/40 bg-muted/30 text-foreground/70'"
              >
                <span v-if="e.is_secret" class="text-[10px]">&#x1f512;</span>
                <span>{{ e.name }}</span>
              </span>
            </div>
          </div>
        </div>
      </div>

      <div v-else class="rounded-2xl border border-border/50 bg-card/60 backdrop-blur-sm p-12 text-center">
        <p class="text-sm text-muted-foreground">No snapshot data</p>
      </div>
    </template>

    <div v-else class="rounded-2xl border border-border/50 bg-card/60 backdrop-blur-sm p-12 text-center">
      <p class="text-sm text-muted-foreground">Job not found</p>
    </div>
  </div>
</template>
