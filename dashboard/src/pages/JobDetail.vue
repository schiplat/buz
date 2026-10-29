<script setup lang="ts">
import { ref, onMounted, onUnmounted, nextTick, computed, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import { ArrowLeft, RefreshCw, ChevronDown, ChevronRight, XCircle, Trash2, Loader2, MinusCircle, AlertTriangle, Info, Lock, CircleHelp, Clock, CheckCircle2, ScrollText } from '@lucide/vue'
import { jobsApi, type Job, type JobApprovalRecord, type LogChunk } from '@/api/jobs'
import { parseUtc, formatLogClock, formatLocalDateTime } from '@/utils/time'
import { formatLogChunkForTerminal } from '@/utils/logFormat'
import { createManagedWebSocket, type ManagedWebSocket } from '@/utils/ws'
import { navLog } from '@/utils/navigation'
import { Terminal } from '@xterm/xterm'
import { FitAddon } from '@xterm/addon-fit'
import '@xterm/xterm/css/xterm.css'
import { useAuthStore } from '@/stores/auth'
import { useAppDialog } from '@/composables/useAppDialog'
import { formatApiError } from '@/utils/error'

const route = useRoute()
const router = useRouter()
const auth = useAuthStore()
const dialog = useAppDialog()
const jobId = route.params.id as string

const job = ref<Job | null>(null)
const logs = ref<LogChunk[]>([])
const loading = ref(true)
/** Set when the logs request failed (e.g. admin-only debug job for non-admins). */
const logsError = ref(false)
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
const TERM_MIN_ROWS = 3
const TERM_MAX_ROWS = 40
/** While a job is still running keep the window compact; only grow past this
 *  once the run is finished and the full output is final. */
const TERM_STREAM_ROWS = 12
/**
 * Fallback cell height when the terminal hasn't measured fonts yet.
 * Must be ≥ real xterm cell height for fontSize 12 (often 19–22px on macOS
 * with JetBrains Mono). Using 18 previously made FitAddon's
 * `Math.floor(height / cellHeight)` return N-1 rows — 3 lines of content
 * only showed 2, forcing a scrollbar.
 */
const TERM_FALLBACK_CELL_HEIGHT = Math.ceil(TERM_FONT_SIZE * 1.75)

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

function isJobLive(): boolean {
  const s = job.value?.status
  return s === 'running' || s === 'pending' || s === 'waiting_approval' || s === 'queued'
}

const showConcurrencyHelp = ref(false)

/**
 * Concurrency panel — always shown when a job is loaded.
 * Without a group: "not configured" + defaults / how to enable.
 * With a group: frozen snapshot policy from trigger time.
 */
const concurrencyInfo = computed(() => {
  const snap = job.value?.snapshot
  if (!job.value) return null
  const group = (snap?.concurrency_group || '').trim()
  if (!group) {
    return {
      configured: false as const,
      group: null as string | null,
      mode: null as string | null,
      maxQueue: null as number | null,
      queueTimeoutSec: null as number | null,
    }
  }
  const mode = (snap?.concurrency_mode || '').trim() || null
  return {
    configured: true as const,
    group,
    mode,
    maxQueue: snap?.concurrency_max_queue ?? null,
    queueTimeoutSec: snap?.concurrency_queue_timeout_seconds ?? null,
  }
})

function concurrencyModeLabel(mode: string | null): string {
  if (!mode) return 'unknown (not frozen in snapshot)'
  switch (mode) {
    case 'queue': return 'queue'
    case 'discard-old': return 'discard-old'
    case 'discard-new': return 'discard-new'
    default: return mode
  }
}

function concurrencyModeHint(configured: boolean, mode: string | null): string {
  if (!configured) {
    return 'No concurrency group on this run — triggers are not serialized. Multiple jobs for this pipeline may run in parallel.'
  }
  switch (mode) {
    case 'queue':
      return 'FIFO: this job waits until earlier pending/running jobs in the group finish. New triggers may still enqueue up to max-queue.'
    case 'discard-old':
      return 'Keep the newest waiting job; older queued jobs are cancelled. A job that is already running is never cancelled.'
    case 'discard-new':
      return 'Reject new triggers with HTTP 409 while any job in the group is pending, running, or queued.'
    default:
      return 'Concurrency group was set; mode was not recorded on this older snapshot. Check the pipeline edit form for the current policy.'
  }
}

function formatQueueTimeout(sec: number | null): string {
  if (sec == null || sec <= 0) return '—'
  if (sec % 3600 === 0) return `${sec / 3600}h`
  if (sec % 60 === 0) return `${sec / 60}m`
  return `${sec}s`
}

/** Actual CSS cell height from xterm's renderer, or a safe fallback. */
function getCellHeight(term: Terminal): number {
  const h = (term as any)._core?._renderService?.dimensions?.css?.cell?.height
  return typeof h === 'number' && h > 0 ? h : TERM_FALLBACK_CELL_HEIGHT
}

/**
 * How many terminal rows the step actually needs.
 *
 * Prefer `baseY + cursorY + 1` (lines written, including soft-wrap) over
 * `buffer.length`, which counts empty viewport filler and oversizes the
 * window after a live stream has grown the terminal.
 */
function neededTerminalRows(stepOrder: number, term?: Terminal): number {
  const logical = countStepLogLines(stepOrder)
  if (!term) return logical
  try {
    const buf = term.buffer.active
    const used = buf.baseY + buf.cursorY + 1
    return Math.max(logical, used)
  } catch {
    return logical
  }
}

/**
 * Size the terminal to its content. While the job is live the height only
 * grows (never shrinks) up to `TERM_STREAM_ROWS` — re-fitting on every streamed
 * chunk would make the window jump around — and the view follows the newest
 * lines. With `force` (job finished) the height snaps to the final line count.
 *
 * Height is derived from the *measured* xterm cell height (not a hard-coded
 * 18px). FitAddon uses `Math.floor(parentHeight / cellHeight)`, so we add a
 * few pixels of slack; otherwise sub-pixel / taller-than-assumed cells clip
 * the last line and force an internal scrollbar.
 */
function fitTerminalHeight(stepOrder: number, opts?: { force?: boolean }) {
  const el = document.getElementById('terminal-el-' + stepOrder) as HTMLDivElement | null
  if (!el) return
  const st = stepTerminals.value.find(t => t.stepOrder === stepOrder)
  const scrollToBottom = () => {
    if (st) {
      requestAnimationFrame(() => {
        try { st.term.scrollToBottom() } catch { /* disposed */ }
      })
    }
  }

  const needed = neededTerminalRows(stepOrder, st?.term)
  const cap = isJobLive() ? TERM_STREAM_ROWS : TERM_MAX_ROWS
  const rows = needed === 0
    ? 4 // room for empty-state message
    : Math.min(cap, Math.max(TERM_MIN_ROWS, needed))

  const cellH = st ? getCellHeight(st.term) : TERM_FALLBACK_CELL_HEIGHT
  // +2px so Math.floor(height / cellH) never underflows by 1 due to subpixels
  const newHeight = Math.ceil(rows * cellH) + 2
  const currentHeight = parseFloat(el.style.height) || 0
  if (!opts?.force && newHeight <= currentHeight) {
    // Height unchanged — just keep following the newest output.
    scrollToBottom()
    return
  }

  el.style.padding = '0'
  el.style.height = `${newHeight}px`
  if (st) {
    requestAnimationFrame(() => {
      try {
        st.fitAddon.fit()
        // If FitAddon still gave fewer rows than needed (padding / rounding),
        // grow once more using the now-fresh cell metrics.
        if (st.term.rows < rows && rows <= cap) {
          const cellH2 = getCellHeight(st.term)
          el.style.height = `${Math.ceil(rows * cellH2) + 4}px`
          st.fitAddon.fit()
        }
        st.term.scrollToBottom()
      } catch { /* disposed */ }
    })
  }
}

/** Smallest failed step order, or null when nothing has failed yet. */
function firstFailedStep(): number | null {
  const failed = (job.value?.step_statuses || [])
    .filter((s: any) => s.status === 'failed')
    .map((s: any) => Number(s.step_order))
    .sort((a: number, b: number) => a - b)
  return failed.length > 0 ? failed[0] : null
}

/**
 * Where a global (step_order=-1) chunk — e.g. a prepare-stage failure like a
 * config bundle 404 — should be surfaced. Prefers the first failed step so the
 * real error is visible next to it; falls back to the smallest known step, or 0.
 */
function resolveGlobalChunkStep(): number {
  const failed = firstFailedStep()
  if (failed != null) return failed
  const smallest = Math.min(...logs.value.filter(c => c.step_order >= 0).map(c => c.step_order), 0)
  return Number.isFinite(smallest) ? smallest : 0
}

const stepLogs = computed(() => {
  const groups: Map<number, LogChunk[]> = new Map()
  const globalChunks: LogChunk[] = []
  for (const chunk of logs.value) {
    if (chunk.step_order === -1) {
      globalChunks.push(chunk)
      continue
    }
    if (!groups.has(chunk.step_order)) groups.set(chunk.step_order, [])
    groups.get(chunk.step_order)!.push(chunk)
  }
  // Runner-side prepare errors (e.g. config bundle download failed) are uploaded
  // with step_order=-1 before any step output exists. Attach them to the step
  // that failed — otherwise the real error stays invisible behind a generic
  // "No output recorded" placeholder.
  if (globalChunks.length > 0) {
    const target = resolveGlobalChunkStep()
    if (!groups.has(target)) groups.set(target, [])
    groups.get(target)!.push(...globalChunks)
  }
  return groups
})

function getStepStatus(stepOrder: number): string {
  if (!job.value?.step_statuses) return 'pending'
  const found = job.value.step_statuses.find((s: any) => Number(s.step_order) === stepOrder)
  return found?.status || 'pending'
}

function isMaskedInput(key: string): boolean {
  const keys = job.value?.snapshot?.masking_input_keys
  return Array.isArray(keys) && keys.includes(key)
}

function displayInputValue(key: string, val: unknown): string {
  if (isMaskedInput(key)) return '***'
  if (val == null) return ''
  return String(val)
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
  logsError.value = false
  // Job metadata and logs are fetched separately: debug-job logs are
  // platform-admin-only, so a logs 403 must not hide the job itself.
  try {
    job.value = await jobsApi.get(jobId)
  } catch (e) { console.error(e) }
  try {
    logs.value = await jobsApi.logs(jobId)
  } catch (e) {
    logsError.value = true
    console.error(e)
  }
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
        // Match WS path: global (-1) prepare errors surface on the failed /
        // active step so poll fallback still shows the real ERROR.
        const writeTargets = new Set<number>()
        if (activeIdx != null) writeTargets.add(activeIdx)
        for (const so of expandedSteps.value) writeTargets.add(so)
        writeTargets.add(resolveGlobalChunkStep())

        const globalStep = resolveGlobalChunkStep()
        let pending = 0
        const finishFits = () => {
          for (const so of writeTargets) fitTerminalHeight(so)
        }
        for (const displayStep of writeTargets) {
          const st = stepTerminals.value.find(t => t.stepOrder === displayStep)
          if (!st) continue
          for (let i = oldLen; i < l.length; i++) {
            const c = l[i]
            const belongs =
              c.step_order === displayStep ||
              (c.step_order === -1 && displayStep === globalStep)
            if (!belongs || st.seenContent.has(c.content)) continue
            st.seenContent.add(c.content)
            const ts = formatLogClock(c.created_at, nowTimeStr())
            pending += 1
            st.term.write(formatLogChunkForTerminal(ts, c), () => {
              pending -= 1
              if (pending === 0) finishFits()
            })
          }
        }
        if (pending === 0) finishFits()
      }
    } catch {
      // Silent for network blips — but a logs 403 on a debug job for a
      // non-admin must surface the admin-only lock instead of spamming.
      if (job.value?.debug_unredact) logsError.value = true
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
  const seen = new Set<string>()
  stepTerminals.value.push({ stepOrder, term, fitAddon, seenContent: seen })

  const force = !isJobLive()
  const finishFit = () => fitTerminalHeight(stepOrder, { force })

  if (chunks.length === 0) {
    finishFit()
    return
  }

  // Wait for xterm to finish parsing every chunk before measuring buffer
  // length — otherwise neededTerminalRows() under-counts and the viewport
  // stays one row short.
  let remaining = chunks.length
  for (const chunk of chunks) {
    const ts = formatLogClock(chunk.created_at, nowTimeStr())
    const text = formatLogChunkForTerminal(ts, chunk)
    seen.add(chunk.content)
    term.write(text, () => {
      remaining -= 1
      if (remaining === 0) finishFit()
    })
  }
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
            if (alive) {
              logs.value = l
              // The output is final now — snap every expanded terminal to its
              // real height (live mode caps it to keep the window stable).
              nextTick(() => {
                for (const so of expandedSteps.value) fitTerminalHeight(so, { force: true })
              })
            }
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
    const displayStep = chunk.step_order === -1 ? resolveGlobalChunkStep() : chunk.step_order
    const st = stepTerminals.value.find(t => t.stepOrder === displayStep)
    if (st && !st.seenContent.has(chunk.content)) {
      st.seenContent.add(chunk.content)
      const ts = formatLogClock(chunk.created_at, nowTimeStr())
      st.term.write(formatLogChunkForTerminal(ts, chunk), () => {
        fitTerminalHeight(displayStep)
      })
    }
  } catch {
    // Old plaintext fallback
    logs.value.push({ step_order: -1, content: raw } as LogChunk)
    const running = stepTerminals.value.find(t => getStepStatus(t.stepOrder) === 'running')
    if (running && !running.seenContent.has(raw)) {
      running.seenContent.add(raw)
      running.term.write(formatLogChunkForTerminal(nowTimeStr(), { content: raw }), () => {
        fitTerminalHeight(running.stepOrder)
      })
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

const approvalBusy = ref(false)

const ROLE_RANK: Record<string, number> = {
  viewer: 1,
  developer: 2,
  maintainer: 3,
  owner: 4,
}

function roleRank(role: string | undefined | null): number {
  if (!role) return 0
  return ROLE_RANK[role.toLowerCase()] ?? 0
}

const canApprove = computed(() => {
  const j = job.value
  if (!j || j.status !== 'waiting_approval') return false
  const protection = j.snapshot?.protection
  // Platform admin may approve/reject even when they triggered the job.
  if (!auth.isPlatformAdmin) {
    if (protection?.forbid_self_approve !== false && j.trigger_user_id && j.trigger_user_id === auth.userId) {
      return false
    }
  }
  if (auth.isPlatformAdmin) return true
  const minRole = protection?.min_approve_role || 'maintainer'
  if (!j.team_id) return false
  const membership = auth.teams.find((t) => t.id === j.team_id)
  if (!membership) return false
  return roleRank(membership.role) >= roleRank(minRole)
})

const canCancelJob = computed(() => {
  const s = job.value?.status
  return s === 'running' || s === 'pending' || s === 'waiting_approval' || s === 'queued'
})

const approvalsReceived = computed(() => job.value?.snapshot?.approval?.received ?? 0)
const approvalsRequired = computed(
  () => job.value?.snapshot?.protection?.require_approvals ?? 1,
)

const approvalList = computed<JobApprovalRecord[]>(() => job.value?.approvals ?? [])
const showApprovalSidebar = computed(
  () => approvalList.value.length > 0 || job.value?.status === 'waiting_approval',
)

function approvalActionClass(action: string): string {
  switch (action) {
    case 'approve':
      return 'bg-emerald-500/10 text-emerald-700 border-emerald-500/25'
    case 'reject':
      return 'bg-red-500/10 text-red-700 border-red-500/25'
    case 'cancel':
      return 'bg-slate-500/10 text-slate-600 border-slate-500/25'
    case 'timeout':
      return 'bg-amber-500/10 text-amber-700 border-amber-500/25'
    default:
      return 'bg-muted text-muted-foreground border-border/50'
  }
}

async function handleApprove() {
  if (!canApprove.value || approvalBusy.value) return
  const comment = await dialog.prompt('Optional note recorded on this approval.', {
    title: 'Approve job',
    label: 'Comment',
    placeholder: 'Optional',
    confirmLabel: 'Approve',
    multiline: true,
  })
  if (comment === null) return
  approvalBusy.value = true
  try {
    clearRefreshTimer()
    await jobsApi.approve(jobId, comment)
    await loadJob()
  } catch (e) {
    console.error(e)
    await dialog.alert(formatApiError(e, 'Failed to approve job'), { title: 'Approve failed' })
  } finally {
    approvalBusy.value = false
  }
}

async function handleReject() {
  if (!canApprove.value || approvalBusy.value) return
  const reason = await dialog.prompt('Optional reason recorded on this rejection.', {
    title: 'Reject job',
    label: 'Reason',
    placeholder: 'Optional',
    confirmLabel: 'Reject',
    multiline: true,
  })
  if (reason === null) return
  approvalBusy.value = true
  try {
    clearRefreshTimer()
    await jobsApi.reject(jobId, reason)
    await loadJob()
  } catch (e) {
    console.error(e)
    await dialog.alert(formatApiError(e, 'Failed to reject job'), { title: 'Reject failed' })
  } finally {
    approvalBusy.value = false
  }
}

async function handleDelete() {
  if (!(await dialog.confirm('Permanently delete this job and all its logs?', {
    title: 'Delete job',
    confirmLabel: 'Delete',
    danger: true,
  }))) return
  try { clearRefreshTimer(); managedWs?.close(); await jobsApi.delete(jobId); router.push(`/pipelines/${job.value?.pipeline_id}/history`) } catch (e) { console.error(e) }
}

function execLabel(t: string): string {
  return { local: 'Runner', ssh: 'SSH', docker: 'Docker', wasm: 'Wasm' }[t] ?? t
}

function stepStatusColor(status: string): string {
  switch (status) {
    case 'success': return 'border-emerald-400/40 bg-emerald-500/4 text-emerald-500'
    case 'failed': return 'border-red-400/40 bg-red-500/4 text-red-500'
    case 'running': return 'border-amber-400/40 bg-amber-500/4 text-amber-500'
    case 'cancelled': return 'border-slate-400/30 bg-slate-500/3 text-slate-400'
    default: return 'border-border/50 bg-muted/30 text-muted-foreground'
  }
}

/** Step number color — the ordinal is always shown as a large standalone
 *  numeral, tinted by status (no badge background, no separate status icon). */
function stepNumberClass(status: string): string {
  switch (status) {
    case 'success': return 'text-emerald-600'
    case 'failed': return 'text-red-600'
    case 'running': return 'text-amber-600'
    case 'cancelled': return 'text-slate-400'
    default: return 'text-muted-foreground/50'
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
      <PageHeader :title="job?.run_number ? `Job #${job.run_number}` : `Job ${jobId}`" :description="job?.pipeline_name || 'Loading...'">
        <template #actions>
          <UiButton v-if="canCancelJob" variant="outline" size="sm" class="border-red-500/30 text-red-600 hover:bg-red-500/10 text-xs px-3 py-1.5" @click="handleCancel">
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
      <div class="flex flex-col lg:flex-row gap-5 items-start">
        <div class="min-w-0 flex-1">
      <!-- Environment used for Variable overrides -->
      <div
        v-if="job.snapshot?.environment"
        class="mb-4 flex items-start gap-2 rounded-xl border border-border/60 bg-muted/30 px-4 py-3 text-sm"
      >
        <span class="text-muted-foreground">
          <span class="font-semibold text-foreground">Environment</span>
          —
          <code class="mono-chip text-xs">{{ job.snapshot.environment }}</code>
          (Variable / Secret defaults overlaid for this run)
        </span>
      </div>

      <!-- Verbose-mode notice: extra detail, redaction intact -->
      <div
        v-if="job.verbose_logs"
        class="mb-4 flex items-start gap-2 rounded-xl border border-sky-500/30 bg-sky-500/10 px-4 py-3 text-sm"
      >
        <Info class="mt-0.5 h-4 w-4 shrink-0 text-sky-600" />
        <span class="text-sky-800">
          <span class="font-semibold">Verbose mode</span> — this run includes extra per-step detail
          (duration, exit code, full step config, stage progress). Secrets remain redacted.
        </span>
      </div>

      <!-- Debug-mode banner: unredacted logs, admin-only, 24h retention -->
      <div
        v-if="job.debug_unredact"
        class="mb-4 flex items-start gap-2 rounded-xl border border-amber-500/40 bg-amber-500/10 px-4 py-3 text-sm"
      >
        <AlertTriangle class="mt-0.5 h-4 w-4 shrink-0 text-amber-600" />
        <span class="text-amber-800">
          <span class="font-semibold">Debug mode</span> — this run's logs were not redacted (plaintext
          secret values may appear). They are visible to platform admins only and are
          <span class="font-semibold">automatically purged 24 hours</span> after the job was created.
        </span>
      </div>

      <!-- Waiting for environment approval -->
      <div
        v-if="job.status === 'waiting_approval'"
        class="mb-4 rounded-xl border border-amber-500/40 bg-amber-500/10 px-4 py-3 text-sm"
      >
        <div class="flex items-start gap-2">
          <AlertTriangle class="mt-0.5 h-4 w-4 shrink-0 text-amber-600" />
          <div class="flex-1 min-w-0">
            <p class="text-amber-900 dark:text-amber-200">
              <span class="font-semibold">Waiting for approval</span>
              —
              environment
              <code class="mono-chip text-xs mx-0.5">{{ job.snapshot?.environment || '—' }}</code>
              requires
              <span class="font-semibold tabular-nums">{{ approvalsReceived }} / {{ approvalsRequired }}</span>
              approval(s) before this job can run.
              <template v-if="job.snapshot?.protection?.min_approve_role">
                Min approve role:
                <span class="font-medium">{{ job.snapshot.protection.min_approve_role }}</span>.
              </template>
            </p>
            <div v-if="canApprove" class="mt-3 flex flex-wrap gap-2">
              <UiButton
                size="sm"
                class="text-xs px-3 py-1.5 bg-emerald-600 hover:bg-emerald-600/90 text-white"
                :disabled="approvalBusy"
                @click="handleApprove"
              >
                <CheckCircle2 class="h-3.5 w-3.5" />
                {{ approvalBusy ? 'Working…' : 'Approve' }}
              </UiButton>
              <UiButton
                variant="outline"
                size="sm"
                class="border-red-500/30 text-red-600 hover:bg-red-500/10 text-xs px-3 py-1.5"
                :disabled="approvalBusy"
                @click="handleReject"
              >
                <XCircle class="h-3.5 w-3.5" /> Reject
              </UiButton>
            </div>
            <p
              v-else
              class="mt-2 text-[12px] text-amber-800/80 dark:text-amber-300/80"
            >
              You do not have permission to approve this job
              <template v-if="!auth.isPlatformAdmin && job.snapshot?.protection?.forbid_self_approve !== false && job.trigger_user_id === auth.userId">
                (self-approve is forbidden)
              </template>.
            </p>
          </div>
        </div>
      </div>

      <!-- Queued: waiting for an earlier job in the concurrency group -->
      <div
        v-if="job.status === 'queued'"
        class="mb-4 flex items-start gap-2 rounded-xl border border-purple-500/30 bg-purple-500/10 px-4 py-3 text-sm"
      >
        <Clock class="mt-0.5 h-4 w-4 shrink-0 text-purple-600" />
        <span class="text-purple-900 dark:text-purple-200">
          <span class="font-semibold">Queued</span> — waiting for an earlier job of this pipeline
          to finish. Only one job in the concurrency group runs at a time; this run starts when
          the active job completes
          <template v-if="concurrencyInfo?.configured">
            (group <code class="font-mono text-xs">{{ concurrencyInfo.group }}</code><template v-if="concurrencyInfo.mode">, mode <code class="font-mono text-xs">{{ concurrencyInfo.mode }}</code></template>).
          </template>
          <template v-else>.</template>
        </span>
      </div>

      <!-- Job Info -->
      <div class="rounded-2xl border border-border/50 bg-card/60 backdrop-blur-xs p-6">
        <h3 class="text-xs font-semibold uppercase tracking-[0.15em] text-muted-foreground/70 mb-5">Job Info</h3>
        <div class="grid grid-cols-2 sm:grid-cols-4 gap-x-8 gap-y-4">
          <div>
            <div class="text-[10px] uppercase tracking-widest text-muted-foreground/50 mb-1.5 font-medium">Status</div>
            <StatusBadge :status="job.status" />
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-widest text-muted-foreground/50 mb-1.5 font-medium">Pipeline</div>
            <div class="text-sm font-medium text-foreground/90">{{ job.pipeline_name || job.pipeline_id }}<span v-if="job.run_number" class="ml-1 text-xs text-muted-foreground">#{{ job.run_number }}</span></div>
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-widest text-muted-foreground/50 mb-1.5 font-medium">Runner</div>
            <div class="font-mono text-xs text-muted-foreground">{{ job.runner_id ? String(job.runner_id).slice(0, 8) + '...' : '\u2014' }}</div>
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-widest text-muted-foreground/50 mb-1.5 font-medium">Trigger</div>
            <div class="font-mono text-xs text-muted-foreground">{{ job.trigger_by }}</div>
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-widest text-muted-foreground/50 mb-1.5 font-medium">Retry</div>
            <div class="font-mono text-sm tabular-nums text-foreground/80">{{ job.retry_times }}</div>
          </div>
          <div v-if="job.resource_profile">
            <div class="text-[10px] uppercase tracking-widest text-muted-foreground/50 mb-1.5 font-medium">Profile</div>
            <div class="font-mono text-xs text-foreground/80">
              {{ job.resource_profile }}
              <span v-if="job.resource_slots" class="text-muted-foreground">({{ job.resource_slots }} slot{{ job.resource_slots > 1 ? 's' : '' }})</span>
            </div>
            <div v-if="job.resource_spec" class="text-[10px] text-muted-foreground">
              {{ job.resource_spec.memory }} · {{ job.resource_spec.cpu }} cpu · pids {{ job.resource_spec.pids }}
            </div>
          </div>
          <div v-if="job.dispatch_reason">
            <div class="text-[10px] uppercase tracking-widest text-muted-foreground/50 mb-1.5 font-medium">Dispatch</div>
            <div class="text-xs text-amber-600">{{ job.dispatch_reason }}</div>
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-widest text-muted-foreground/50 mb-1.5 font-medium">Started</div>
            <div class="text-xs text-muted-foreground">{{ formatLocalDateTime(job.start_time) }}</div>
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-widest text-muted-foreground/50 mb-1.5 font-medium">Ended</div>
            <div class="text-xs text-muted-foreground">{{ formatLocalDateTime(job.end_time) }}</div>
          </div>
          <div>
            <div class="text-[10px] uppercase tracking-widest text-muted-foreground/50 mb-1.5 font-medium">Created</div>
            <div class="text-xs text-muted-foreground">{{ formatLocalDateTime(job.created_at) }}</div>
          </div>
        </div>

        <!-- Concurrency: always show — configured policy or "not set" + knobs -->
        <div v-if="concurrencyInfo" class="mt-5 pt-5 border-t border-border/30">
          <div class="flex items-center gap-2 mb-3">
            <h4 class="text-[10px] uppercase tracking-widest text-muted-foreground/50 font-medium">Concurrency</h4>
            <button
              type="button"
              class="inline-flex items-center text-muted-foreground/50 hover:text-foreground transition-colors"
              :aria-expanded="showConcurrencyHelp"
              aria-label="Concurrency policy help"
              @click="showConcurrencyHelp = !showConcurrencyHelp"
            >
              <CircleHelp class="h-3.5 w-3.5" />
            </button>
          </div>

          <template v-if="concurrencyInfo.configured">
            <div class="flex flex-wrap items-center gap-2 text-sm">
              <span class="text-xs text-muted-foreground">Group</span>
              <code class="text-[11px] font-mono bg-muted rounded-md px-2 py-0.5 text-foreground/80">{{ concurrencyInfo.group }}</code>
              <span class="text-xs text-muted-foreground ml-1">Mode</span>
              <span
                class="text-[11px] font-semibold rounded-md px-2 py-0.5 border"
                :class="{
                  'bg-amber-500/10 text-amber-700 border-amber-500/20': concurrencyInfo.mode === 'queue',
                  'bg-blue-500/10 text-blue-700 border-blue-500/20': concurrencyInfo.mode === 'discard-new',
                  'bg-emerald-500/10 text-emerald-700 border-emerald-500/20': concurrencyInfo.mode === 'discard-old',
                  'bg-muted text-muted-foreground border-border/50': !concurrencyInfo.mode,
                }"
              >{{ concurrencyModeLabel(concurrencyInfo.mode) }}</span>
              <template v-if="concurrencyInfo.mode === 'queue'">
                <span class="text-xs text-muted-foreground ml-1">Max queue</span>
                <span class="text-[11px] font-mono tabular-nums text-foreground/80">{{ concurrencyInfo.maxQueue ?? '—' }}</span>
                <span class="text-xs text-muted-foreground ml-1">Queue timeout</span>
                <span class="text-[11px] font-mono tabular-nums text-foreground/80">{{ formatQueueTimeout(concurrencyInfo.queueTimeoutSec) }}</span>
              </template>
            </div>
          </template>
          <template v-else>
            <div class="flex flex-wrap items-center gap-2 text-sm">
              <span class="text-xs text-muted-foreground">Status</span>
              <span class="text-[11px] font-semibold rounded-md px-2 py-0.5 border bg-muted text-muted-foreground border-border/50">not configured</span>
              <span class="text-xs text-muted-foreground ml-1">Default mode if enabled</span>
              <span class="text-[11px] font-mono rounded-md px-2 py-0.5 border bg-emerald-500/10 text-emerald-700 border-emerald-500/20">discard-old</span>
              <span class="text-xs text-muted-foreground ml-1">Max queue</span>
              <span class="text-[11px] font-mono tabular-nums text-foreground/80">3</span>
              <span class="text-xs text-muted-foreground ml-1">Queue timeout</span>
              <span class="text-[11px] font-mono tabular-nums text-foreground/80">30m</span>
            </div>
          </template>

          <p class="mt-2 text-xs text-muted-foreground leading-relaxed">
            {{ concurrencyModeHint(concurrencyInfo.configured, concurrencyInfo.mode) }}
          </p>
          <div
            v-if="showConcurrencyHelp"
            class="mt-3 rounded-lg border border-border/50 bg-muted/30 px-3 py-2.5 text-xs text-muted-foreground leading-relaxed space-y-2"
          >
            <p class="font-medium text-foreground/80">
              {{ concurrencyInfo.configured ? 'What you can adjust (future triggers only)' : 'How to enable concurrency' }}
            </p>
            <ul class="list-disc pl-4 space-y-1">
              <li><code class="font-mono text-[10px]">concurrency.group</code> — required to enable; shared lock name for this pipeline</li>
              <li><code class="font-mono text-[10px]">mode</code> — <code class="font-mono text-[10px]">discard-old</code> (YAML default), <code class="font-mono text-[10px]">queue</code>, or <code class="font-mono text-[10px]">discard-new</code></li>
              <li><code class="font-mono text-[10px]">max-queue</code> (1–100, default 3) — only when mode is <code class="font-mono text-[10px]">queue</code></li>
              <li><code class="font-mono text-[10px]">queue-timeout</code> (default 30m) — auto-cancel queued jobs after this; only for <code class="font-mono text-[10px]">queue</code></li>
            </ul>
            <p>
              Set these on the pipeline
              <router-link
                :to="'/pipelines/' + job.pipeline_id + '/definition'"
                class="text-primary underline underline-offset-2 hover:text-primary/80"
              >Definition → Edit (Concurrency)</router-link>
              <template v-if="concurrencyInfo.configured">
                . This job keeps the policy frozen above; edits apply to new runs only.
              </template>
              <template v-else>
                , or in YAML under <code class="font-mono text-[10px]">concurrency:</code>. Applies to new runs only.
              </template>
            </p>
          </div>
        </div>
      </div>

      <!-- Pipeline Snapshot + Steps with embedded terminals -->
      <div v-if="job.snapshot && job.snapshot.tasks && job.snapshot.tasks.length > 0" class="rounded-2xl border border-border/50 bg-card/60 backdrop-blur-xs">
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
            <span class="text-[10px] uppercase tracking-widest text-muted-foreground/50 font-medium">Runner</span>
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
                    ? 'border-primary/30 bg-primary/3 shadow-xs'
                    : (getStepStatus(t.step_order) === 'pending'
                      ? ''
                      : 'border-border/40 bg-transparent hover:bg-muted/40 hover:border-border')
                ]"
                @click="getStepStatus(t.step_order) !== 'pending' && toggleStep(t.step_order)"
              >
                <!-- Step number (large standalone numeral, tinted by status) -->
                <div class="shrink-0 w-8 flex items-center justify-center">
                  <span
                    class="text-xl font-semibold tabular-nums leading-none"
                    :class="stepNumberClass(getStepStatus(t.step_order))"
                  >{{ t.step_order + 1 }}</span>
                </div>

                <!-- Step main content -->
                <div class="flex-1 min-w-0">
                  <div class="flex items-center gap-2 flex-wrap">
                    <span class="text-[13px] font-semibold text-foreground/90 truncate max-w-[280px]">{{ t.name }}</span>
                    <span class="text-[9px] font-mono uppercase tracking-wider rounded-md border border-border/40 px-1.5 py-0 text-muted-foreground/60">{{ execLabel(t.exec_type) }}</span>
                    <span v-if="t.host_id" class="text-[9px] font-mono rounded-md bg-muted px-1.5 py-0 text-muted-foreground/60 truncate max-w-[120px]">{{ t.host_id }}</span>
                  </div>
                  <div class="font-mono text-[11px] text-muted-foreground/60 whitespace-pre-wrap break-all leading-relaxed mt-1">{{ t.command_template }}</div>
                  <div v-if="t.marketplace" class="mt-2 rounded-lg border border-border/40 bg-muted/20 px-2.5 py-2 text-[10px] font-mono text-muted-foreground/70 space-y-1">
                    <div class="flex flex-wrap items-center gap-x-2 gap-y-1">
                      <span class="font-semibold text-foreground/70">{{ t.marketplace.uses_ref }}</span>
                      <span>v{{ t.marketplace.resolved_version }}</span>
                      <span :class="t.marketplace.reviewed ? 'text-emerald-600' : 'text-amber-600'">{{ t.marketplace.reviewed ? 'reviewed' : 'unreviewed' }}</span>
                    </div>
                    <div class="break-all">sha256:{{ t.marketplace.content_sha256 }}</div>
                    <div class="break-all">source: {{ t.marketplace.source }}</div>
                    <div v-if="t.capabilities?.length" class="font-semibold text-red-600" title="Runner admins authorize capabilities separately in local Runner configuration.">Requests (not grants): {{ t.capabilities.join(', ') }}</div>
                    <div v-if="t.capabilities?.includes('docker-socket')" class="text-amber-700">High privilege: Docker socket access gives root-equivalent control of the Runner's Docker host. A Runner administrator must enable <code>docker_socket_allowed = true</code> locally.</div>
                    <div v-if="t.marketplace.requires_buz" class="text-muted-foreground/60">Requires Buz {{ t.marketplace.requires_buz }}</div>
                    <div v-if="t.marketplace.inputs?.length" class="space-y-0.5">
                      <div class="text-[9px] uppercase tracking-wide text-muted-foreground/60">Plugin inputs</div>
                      <div v-for="input in t.marketplace.inputs" :key="input.name" class="break-all">{{ input.name }}<span v-if="input.required"> (required)</span><span v-if="input.description"> — {{ input.description }}</span></div>
                    </div>
                  </div>
                  <div v-else-if="t.capabilities?.length" class="mt-2 rounded-lg border border-red-500/25 bg-red-500/5 px-2.5 py-2 text-[10px] font-mono font-semibold text-red-600" title="Execution capabilities are requests only; the Runner authorizes them locally.">High privilege request (not grant): {{ t.capabilities.join(', ') }}<div v-if="t.capabilities.includes('docker-socket')" class="mt-1 font-normal text-amber-700">Docker socket access gives root-equivalent control of the Runner's Docker host; Runner-admin local authorization is required.</div></div>
                  <div v-if="t.fail_strategy === 'continue'" class="mt-1.5">
                    <span class="inline-flex items-center gap-1 text-[10px] text-amber-600 bg-amber-500/6 border border-amber-500/20 rounded-md px-2 py-0 font-medium">continue-on-error</span>
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
                  <span class="text-[9px] font-mono uppercase rounded-sm px-1.5 py-0 border border-border/50 text-muted-foreground/70 leading-normal">{{ execLabel(t.exec_type) }}</span>
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
                    <div v-if="logsError" class="text-center space-y-1.5">
                      <Lock class="h-3.5 w-3.5 mx-auto text-amber-600" />
                      <p class="text-[11px] text-amber-700/80">Logs are admin-only — this run used Debug mode</p>
                    </div>
                    <div v-else class="text-center space-y-1.5">
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
            <span class="text-[10px] uppercase tracking-widest text-muted-foreground/50 font-medium">Trigger Inputs</span>
            <div class="flex flex-wrap gap-1.5 mt-2">
              <span
                v-for="(val, key) in job.snapshot.inputs" :key="key"
                class="inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-[11px] font-mono"
                :class="isMaskedInput(String(key))
                  ? 'border-violet-500/20 bg-violet-500/4 text-violet-700/90'
                  : 'border-primary/15 bg-primary/3'"
              >
                <span class="text-muted-foreground/70">{{ key }}</span>
                <span
                  v-if="isMaskedInput(String(key))"
                  class="text-[9px] uppercase tracking-wide font-sans font-medium text-violet-600/80"
                >masking</span>
                <span class="text-foreground/70 font-medium">{{ displayInputValue(String(key), val) }}</span>
              </span>
            </div>
          </div>

          <!-- Env & Secrets -->
          <div v-if="job.snapshot.env && job.snapshot.env.length > 0" class="pt-1 border-t border-border/20">
            <span class="text-[10px] uppercase tracking-widest text-muted-foreground/50 font-medium">Environment &amp; Secrets</span>
            <div class="flex flex-wrap gap-1.5 mt-2">
              <span
                v-for="e in job.snapshot.env" :key="e.name"
                class="inline-flex items-center gap-1 rounded-md border px-2 py-1 text-[11px] font-mono"
                :class="e.is_secret
                  ? 'border-amber-400/15 bg-amber-400/3 text-amber-500/80'
                  : 'border-border/40 bg-muted/30 text-foreground/70'"
              >
                <span v-if="e.is_secret" class="text-[10px]">&#x1f512;</span>
                <span>{{ e.name }}</span>
              </span>
            </div>
          </div>
        </div>
      </div>

      <div v-else class="rounded-2xl border border-border/50 bg-card/60 backdrop-blur-xs p-12 text-center">
        <p class="text-sm text-muted-foreground">No snapshot data</p>
      </div>
        </div>

        <!-- Approval history — right sidebar (no pagination: few events per job) -->
        <aside
          v-if="showApprovalSidebar"
          class="w-full lg:w-88 xl:w-96 shrink-0 lg:sticky lg:top-4 rounded-2xl border border-border/50 bg-card shadow-xs overflow-hidden flex flex-col max-h-[min(70vh,40rem)]"
        >
          <div class="px-4 py-3 border-b border-border/30 shrink-0">
            <div class="flex items-center gap-2">
              <ScrollText class="h-3.5 w-3.5 text-muted-foreground" />
              <p class="text-[12px] font-medium">Approval history</p>
              <span
                v-if="approvalList.length > 0"
                class="ml-auto text-[10px] font-mono tabular-nums text-muted-foreground bg-muted rounded-md px-1.5 py-0.5"
              >{{ approvalList.length }}</span>
            </div>
            <p class="text-[11px] text-muted-foreground mt-1">
              Append-only trail (approve / reject / cancel / timeout).
            </p>
          </div>
          <div class="scrollbar-sidebar flex-1 overflow-y-auto min-h-0">
            <div v-if="approvalList.length === 0" class="px-4 py-8 text-center text-[12px] text-muted-foreground">
              No approval events yet.
            </div>
            <ul v-else class="divide-y divide-border/20">
              <li
                v-for="a in approvalList"
                :key="a.id"
                class="px-4 py-3 hover:bg-muted/5"
              >
                <div class="flex items-center gap-2 flex-wrap">
                  <span
                    class="inline-flex items-center rounded-md border px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-wide"
                    :class="approvalActionClass(a.action)"
                  >{{ a.action }}</span>
                  <span class="text-[12px] font-medium truncate">{{ a.username || a.user_id || '—' }}</span>
                  <span class="ml-auto text-[10px] font-mono text-muted-foreground whitespace-nowrap">
                    {{ a.created_at ? formatLocalDateTime(a.created_at) : '—' }}
                  </span>
                </div>
                <p
                  v-if="a.comment"
                  class="mt-1.5 text-[11px] text-muted-foreground whitespace-pre-wrap wrap-break-word"
                >{{ a.comment }}</p>
              </li>
            </ul>
          </div>
        </aside>
      </div>
    </template>

    <div v-else class="rounded-2xl border border-border/50 bg-card/60 backdrop-blur-xs p-12 text-center">
      <p class="text-sm text-muted-foreground">Job not found</p>
    </div>
  </div>
</template>
