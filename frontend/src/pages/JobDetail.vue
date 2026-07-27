<script setup lang="ts">
import { ref, onMounted, onUnmounted, nextTick, computed, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import { ArrowLeft, RefreshCw, ChevronDown, ChevronRight, XCircle, Trash2, CheckCircle2, Loader2, MinusCircle } from '@lucide/vue'
import { jobsApi, type Job, type LogChunk } from '@/api/jobs'
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

// ── Per-step terminals ──
interface StepTerminal {
  stepOrder: number
  term: Terminal
  fitAddon: FitAddon
  seenContent: Set<string>
}

const stepTerminals = ref<StepTerminal[]>([])
const expandedSteps = ref<Set<number>>(new Set())
let ws: WebSocket | null = null
const targetStep = ref<number | null>(null)

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
  const s = Math.max(0, Math.floor((b - a) / 1000))
  if (s < 60) return `${s}s`
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`
}

function parseUtc(ts: string): number {
  return Date.parse(ts.trim().replace(' ', 'T') + '.000Z')
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
  clearRefreshTimer()
  ws?.close()
  ws = null
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
  refreshTimer = setInterval(async () => {
    const s = job.value?.status
    if (s !== 'running' && s !== 'pending') { clearRefreshTimer(); return }
    try {
      const j = await jobsApi.get(jobId)
      job.value = j

      const activeIdx = findActiveStep(j.step_statuses)
      const completed = getCompletedSteps(j.step_statuses)

      // Auto-collapse completed steps (keep only the running one expanded)
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

      // Auto-expand the currently running step
      if (activeIdx != null && !expandedSteps.value.has(activeIdx)) {
        expandedSteps.value.add(activeIdx)
        targetStep.value = activeIdx
        router.replace({ query: { step: activeIdx } })
        nextTick(() => {
          initSingleTerminal(activeIdx)
          const st = stepTerminals.value.find(t => t.stepOrder === activeIdx)
          if (st) st.fitAddon.fit()
        })
      }
      if (j.status !== 'running' && j.status !== 'pending') clearRefreshTimer()
    } catch { /* silent */ }
  }, 3000)
}

function clearRefreshTimer() {
  if (refreshTimer) { clearInterval(refreshTimer); refreshTimer = null }
}

function initSingleTerminal(stepOrder: number) {
  // Skip if already created
  if (stepTerminals.value.find(t => t.stepOrder === stepOrder)) return

  const el = document.getElementById('terminal-el-' + stepOrder) as HTMLDivElement | null
  if (!el) return

  const fitAddon = new FitAddon()
  const term = new Terminal({
    cursorBlink: false, disableStdin: true, fontSize: 12,
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
  })
  term.loadAddon(fitAddon)
  term.open(el)
  fitAddon.fit()

  const seen = new Set<string>()
  for (const chunk of stepLogs.value.get(stepOrder) || []) {
    const ts = chunk.created_at ? chunk.created_at.slice(11, 19) : '--:--:--'
    term.write(`\x1b[2m[${ts}]\x1b[0m ${chunk.content.replace(/\n/g, '\r\n')}`)
    seen.add(chunk.content)
  }
  stepTerminals.value.push({ stepOrder, term, fitAddon, seenContent: seen })
}

function startWebSocket() {
  ws?.close()
  const token = localStorage.getItem('buz_token') || ''
  const protocol = window.location.protocol === 'https:' ? 'wss' : 'ws'
  const wsUrl = `${protocol}://${window.location.host}/api/v1/jobs/${jobId}/logs/stream?token=${encodeURIComponent(token)}`
  ws = new WebSocket(wsUrl)
  ws.onmessage = (event) => {
    try {
      const data = JSON.parse(event.data) as LogChunk
      // Always buffer the chunk so it survives terminal destruction / recreation
      logs.value.push(data)
      // Write to the live terminal if it exists
      const st = stepTerminals.value.find(t => t.stepOrder === data.step_order)
      if (st && !st.seenContent.has(data.content)) {
        st.seenContent.add(data.content)
        const ts = data.created_at ? data.created_at.slice(11, 19) : nowTimeStr()
        st.term.write(`\x1b[2m[${ts}]\x1b[0m ${data.content.replace(/\n/g, '\r\n')}`)
      }
    } catch {
      // Old plaintext fallback — route to the currently running step
      const running = stepTerminals.value.find(t => getStepStatus(t.stepOrder) === 'running')
      if (running && !running.seenContent.has(event.data)) {
        running.seenContent.add(event.data)
        running.term.write(`\x1b[2m[${nowTimeStr()}]\x1b[0m ${(event.data as string).replace(/\n/g, '\r\n')}`)
      }
    }
  }
}

function toggleStep(stepOrder: number) {
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
      const st = stepTerminals.value.find(t => t.stepOrder === stepOrder)
      if (st) st.fitAddon.fit()
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
      const st = stepTerminals.value.find(t => t.stepOrder === n)
      if (st) st.fitAddon.fit()
    })
  }
})

async function handleCancel() {
  try { clearRefreshTimer(); await jobsApi.cancel(jobId); await loadJob() } catch (e) { console.error(e) }
}

async function handleDelete() {
  if (!window.confirm('Permanently delete this job and all its logs?')) return
  try { clearRefreshTimer(); ws?.close(); await jobsApi.delete(jobId); router.push(`/pipelines/${job.value?.pipeline_id}/history`) } catch (e) { console.error(e) }
}

function execLabel(t: string): string {
  return { local: 'Runner', ssh: 'SSH', docker: 'Docker', wasm: 'Wasm' }[t] ?? t
}
</script>

<template>
  <div class="space-y-6">
    <div class="flex items-center gap-3">
      <button class="inline-flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground transition-colors" @click="router.push('/pipelines/' + (job?.pipeline_id || '') + '/history')">
        <ArrowLeft class="h-4 w-4" />
      </button>
      <PageHeader :title="`Job ${jobId.slice(0, 8)}...`" :description="job?.pipeline_name || 'Loading...'">
        <template #actions>
          <UiButton v-if="job?.status === 'running' || job?.status === 'pending'" variant="outline" size="sm" class="border-red-500/30 text-red-600 hover:bg-red-500/10" @click="handleCancel">
            <XCircle class="h-3.5 w-3.5" /> Cancel
          </UiButton>
          <UiButton variant="outline" size="sm" class="border-red-500/20 text-red-500 hover:bg-red-500/10" @click="handleDelete">
            <Trash2 class="h-3.5 w-3.5" /> Delete
          </UiButton>
          <UiButton variant="outline" size="sm" @click="loadJob" :disabled="loading">
            <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': loading }" /> Refresh
          </UiButton>
        </template>
      </PageHeader>
    </div>

    <div v-if="loading && !job" class="surface-card p-8 text-center type-meta">Loading...</div>

    <template v-else-if="job">
      <!-- Job Info -->
      <div class="surface-card p-5">
        <h3 class="text-sm font-semibold tracking-tight mb-4">Job Info</h3>
        <div class="grid grid-cols-2 sm:grid-cols-4 gap-4 text-sm">
          <div><div class="type-eyebrow mb-0.5">Status</div><StatusBadge :status="job.status" /></div>
          <div><div class="type-eyebrow mb-0.5">Pipeline</div><div class="font-medium">{{ job.pipeline_name || job.pipeline_id }}</div></div>
          <div><div class="type-eyebrow mb-0.5">Runner</div><div class="font-mono text-xs text-muted-foreground">{{ job.runner_id ? String(job.runner_id).slice(0, 8) + '...' : '\u2014' }}</div></div>
          <div><div class="type-eyebrow mb-0.5">Trigger</div><div class="font-mono text-xs">{{ job.trigger_by }}</div></div>
          <div><div class="type-eyebrow mb-0.5">Retry</div><div class="font-mono">{{ job.retry_times }}</div></div>
          <div><div class="type-eyebrow mb-0.5">Started</div><div class="text-xs text-muted-foreground">{{ job.start_time || '\u2014' }}</div></div>
          <div><div class="type-eyebrow mb-0.5">Ended</div><div class="text-xs text-muted-foreground">{{ job.end_time || '\u2014' }}</div></div>
          <div><div class="type-eyebrow mb-0.5">Created</div><div class="text-xs text-muted-foreground">{{ job.created_at }}</div></div>
        </div>
      </div>

      <!-- Pipeline Snapshot + Execution Logs -->
      <div v-if="job.snapshot && job.snapshot.tasks && job.snapshot.tasks.length > 0" class="surface-card">
        <div class="flex items-center justify-between px-5 py-3">
          <div class="flex items-center gap-2">
            <h3 class="text-sm font-semibold tracking-tight">Steps</h3>
            <span class="text-[10px] text-muted-foreground mono-chip bg-muted">{{ job.snapshot.tasks.length }}</span>
            <span class="text-[10px] text-muted-foreground mono-chip bg-muted">Frozen</span>
            <span v-if="job.snapshot.triggered_at" class="text-[11px] text-muted-foreground font-mono ml-1">{{ job.snapshot.triggered_at.slice(0, 19).replace('T', ' ') }}</span>
          </div>
        </div>

        <div class="border-t border-border/60 px-5 py-3 space-y-3">
          <!-- Runner Labels -->
          <div v-if="job.snapshot.runner_labels && job.snapshot.runner_labels.length > 0">
            <span class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Runner Labels</span>
            <div class="flex flex-wrap gap-1 mt-1">
              <span v-for="l in job.snapshot.runner_labels" :key="l" class="mono-chip">{{ l }}</span>
            </div>
          </div>

          <!-- Step cards with inline terminals -->
          <div class="space-y-2">
            <div v-for="t in job.snapshot.tasks" :key="t.step_order" :id="'step-' + t.step_order">
              <div
                class="flex items-center gap-3 rounded-lg border px-4 py-3 transition-all cursor-pointer"
                :class="[targetStep === t.step_order ? 'ring-2 ring-primary/30 border-primary/50 bg-primary/[0.03]' : 'border-border/60 bg-muted/30 hover:bg-muted/50']"
                @click="toggleStep(t.step_order)"
              >
                <CheckCircle2 v-if="getStepStatus(t.step_order) === 'success'" class="h-4 w-4 shrink-0 text-emerald-500" />
                <XCircle v-else-if="getStepStatus(t.step_order) === 'failed'" class="h-4 w-4 shrink-0 text-red-500" />
                <Loader2 v-else-if="getStepStatus(t.step_order) === 'running'" class="h-4 w-4 shrink-0 text-amber-500 animate-spin" />
                <MinusCircle v-else-if="getStepStatus(t.step_order) === 'cancelled'" class="h-4 w-4 shrink-0 text-slate-400" />
                <span v-else class="inline-flex items-center justify-center h-4 w-4 shrink-0 rounded-full bg-slate-100 dark:bg-slate-800 text-[10px] font-bold text-slate-400 tabular-nums leading-none">{{ t.step_order + 1 }}</span>

                <div class="flex-1 min-w-0">
                  <div class="flex items-center gap-1.5">
                    <span class="text-[13px] font-semibold">{{ t.name }}</span>
                    <span class="mono-chip text-[9px]">{{ execLabel(t.exec_type) }}</span>
                    <span v-if="t.host_id" class="mono-chip text-[9px]">{{ t.host_id }}</span>
                  </div>
                  <div class="font-mono text-[10px] text-muted-foreground truncate max-w-[400px] mt-0.5">{{ t.command_template }}</div>
                  <div v-if="t.fail_strategy === 'continue'" class="mt-1">
                    <span class="text-[10px] text-amber-600 bg-amber-500/10 rounded px-1.5 py-0.5">continue-on-error</span>
                  </div>
                </div>

                <div class="flex items-center gap-2 shrink-0">
                  <span class="text-[10px] font-mono tabular-nums" :class="getStepStatus(t.step_order) === 'running' ? 'text-amber-600' : 'text-muted-foreground'">{{ fmtStepDuration(t.step_order) }}</span>
                  <ChevronDown v-if="expandedSteps.has(t.step_order)" class="h-3.5 w-3.5 text-muted-foreground" />
                  <ChevronRight v-else class="h-3.5 w-3.5 text-muted-foreground" />
                </div>
              </div>

              <!-- Inline terminal -->
              <div v-if="expandedSteps.has(t.step_order)" class="mt-2 rounded-lg border border-border/60 overflow-hidden">
                <div :id="'terminal-el-' + t.step_order" class="h-[300px] w-full" />
                <div v-if="(stepLogs.get(t.step_order) || []).length === 0 && job.status === 'running'" class="text-[10px] text-center text-muted-foreground/50 pb-2">Waiting for first log chunk...</div>
                <div v-else-if="(stepLogs.get(t.step_order) || []).length === 0 && job.status !== 'running'" class="text-[10px] text-center text-muted-foreground/50 pb-2">No log records for this step</div>
              </div>
            </div>
          </div>

          <!-- Trigger Inputs -->
          <div v-if="job.snapshot.inputs && Object.keys(job.snapshot.inputs).length > 0" class="pt-2 border-t border-border/30">
            <span class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Trigger Inputs</span>
            <div class="flex flex-wrap gap-1.5 mt-1">
              <span v-for="(val, key) in job.snapshot.inputs" :key="key" class="rounded-md px-2 py-0.5 text-[11px] font-mono border border-primary/30 bg-primary/5">{{ key }} = {{ val }}</span>
            </div>
          </div>

          <!-- Env & Secrets -->
          <div v-if="job.snapshot.env && job.snapshot.env.length > 0">
            <span class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider">Environment &amp; Secrets</span>
            <div class="flex flex-wrap gap-1.5 mt-1">
              <span v-for="e in job.snapshot.env" :key="e.name" class="rounded-md px-2 py-0.5 text-[11px] font-mono border" :class="e.is_secret ? 'border-amber-500/30 bg-amber-500/5 text-amber-600' : 'border-border/60 bg-muted/40 text-foreground'">{{ e.is_secret ? '🔒 ' : '' }}{{ e.name }}</span>
            </div>
          </div>
        </div>
      </div>
      <div v-else class="surface-card p-8 text-center type-meta">No snapshot data</div>
    </template>
    <div v-else class="surface-card p-8 text-center type-meta">Job not found</div>
  </div>
</template>
