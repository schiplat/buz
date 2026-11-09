<script setup lang="ts">
import { ref, onMounted, onUnmounted, nextTick, computed, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import { ArrowLeft, RefreshCw, ChevronRight, XCircle, Trash2, Loader2, MinusCircle, AlertTriangle, Info, Lock, Clock, CheckCircle2, ScrollText, Copy, Check, Server, List, Sliders, FileCode2 } from '@lucide/vue'
import { jobsApi, type Job, type JobApprovalRecord, type LogChunk, type SnapshotTask } from '@/api/jobs'
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
import { copyText } from '@/utils/copyText'

const route = useRoute()
const router = useRouter()
const auth = useAuthStore()
const dialog = useAppDialog()
const jobId = route.params.id as string

const job = ref<Job | null>(null)
const logs = ref<LogChunk[]>([])
/**
 * Log reads page from the END: initial load takes the last LOG_TAIL rows,
 * "load earlier" back-pages with an end-exclusive (step_order, chunk_index)
 * cursor (rows are append-only, so cursors are stable). Earlier rows the
 * user pulls in are preserved by mergeTailWindow when the tail refreshes.
 */
const LOG_TAIL = 1000
const earlierExhausted = ref(false)
const loadingEarlier = ref(false)
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
const copiedLogStep = ref<number | null>(null)
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
    } else if (job.value?.status === 'failed') {
      // A finished failure should open on its error instead of a metadata wall.
      const failed = firstFailedStep()
      const step = failed != null && failed >= 0
        ? failed
        : (logs.value.length > 0 ? resolveGlobalChunkStep() : null)
      if (step != null && step >= 0 && (job.value?.snapshot?.tasks || []).length > 0) {
        targetStep.value = step
        router.replace({ query: { step } })
      }
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

function chunkBefore(a: LogChunk, b: LogChunk): boolean {
  return a.step_order < b.step_order || (a.step_order === b.step_order && a.chunk_index < b.chunk_index)
}

/** Replace the tail window while keeping earlier rows the user loaded. */
function mergeTailWindow(fresh: LogChunk[]) {
  if (fresh.length === 0) return
  const first = fresh[0]
  const kept = logs.value.filter((c) => chunkBefore(c, first))
  logs.value = [...kept, ...fresh]
}

async function loadEarlierLogs() {
  const first = logs.value[0]
  if (!first || earlierExhausted.value || loadingEarlier.value) return
  loadingEarlier.value = true
  try {
    const prev = await jobsApi.logs(jobId, {
      tail: LOG_TAIL,
      beforeStep: first.step_order,
      beforeChunk: first.chunk_index,
    })
    if (prev.length < LOG_TAIL) earlierExhausted.value = true
    if (prev.length > 0) logs.value = [...prev, ...logs.value]
  } catch (e) { console.error(e) }
  finally { loadingEarlier.value = false }
}

async function loadJob() {
  loading.value = true
  logsError.value = false
  // Job metadata and logs are fetched separately: debug-job logs are
  // platform-admin-only, so a logs 403 must not hide the job itself.
  try {
    job.value = await jobsApi.get(jobId)
  } catch (e) { console.error(e) }
  try {
    logs.value = await jobsApi.logs(jobId, { tail: LOG_TAIL })
    if (logs.value.length < LOG_TAIL) earlierExhausted.value = true
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
      const l = await jobsApi.logs(jobId, { signal: pollAbort.signal, timeout: 5_000, tail: LOG_TAIL })
      if (!alive) return
      if (l.length > 0) {
        mergeTailWindow(l)
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
          for (const c of l) {
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

/**
 * Snap the selection end back by one cell when the mouse is released inside
 * the RIGHT half of the last intended character.
 *
 * xterm computes the drag endpoint as ceil((x + cellWidth/2) / cellWidth):
 * releasing in the right half of char N pushes the selection end to cell N+1,
 * so a copy carries one character past the released char (both drag
 * directions — the reported repro). On mouseup we detect that half-cell grab
 * from the release pixel and move the model's end back to the release cell,
 * keeping highlight and copy consistent.
 */
function attachSelectionSnap(term: Terminal, el: HTMLElement) {
  const onMouseUp = (event: MouseEvent) => {
    if (event.button !== 0 || !term.hasSelection()) return
    const core = (term as any)._core
    const cellWidth: number | undefined = core?._renderService?.dimensions?.css?.cell?.width
    const selectionService = core?.selectionService
    const model = selectionService?._model
    const modelEnd = model?.selectionEnd as [number, number] | undefined
    if (typeof cellWidth !== 'number' || cellWidth <= 0 || !modelEnd) return
    const rect = el.getBoundingClientRect()
    const releaseX = event.clientX - rect.left
    const releaseCol = Math.floor(releaseX / cellWidth)
    // Half-cell grab: model end advanced one cell past the released char.
    if (modelEnd[0] === releaseCol + 1 && releaseX - releaseCol * cellWidth > cellWidth / 2) {
      model.selectionEnd = [releaseCol, modelEnd[1]]
      selectionService.refresh(true)
    }
  }
  el.addEventListener('mouseup', onMouseUp)
}

function initSingleTerminal(stepOrder: number) {

  const el = document.getElementById('terminal-el-' + stepOrder) as HTMLDivElement | null
  if (!el) return

  fitTerminalHeight(stepOrder)

  const fitAddon = new FitAddon()
  const term = new Terminal({
    cursorBlink: false, disableStdin: true, fontSize: TERM_FONT_SIZE,
    cursorStyle: 'bar',
    cursorInactiveStyle: 'none',
    fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', monospace",
    theme: {
      background: '#0d1117', foreground: '#c9d1d9',
      cursor: '#0d1117', cursorAccent: '#c9d1d9',
      selectionBackground: '#264f78', selectionForeground: '#f0f6fc',
      selectionInactiveBackground: '#264f78',
      black: '#484f58', red: '#ff7b72', green: '#3fb950', yellow: '#d29922',
      blue: '#58a6ff', magenta: '#bc8cff', cyan: '#39c5cf', white: '#b1bac4',
      brightBlack: '#6e7681', brightRed: '#ffa198', brightGreen: '#56d364',
      brightYellow: '#e3b341', brightBlue: '#79c0ff', brightMagenta: '#d2a8ff',
      brightCyan: '#56d4dd', brightWhite: '#f0f6fc',
    },
    allowProposedApi: true,
    scrollback: 5000,
  })
  attachSelectionSnap(term, el)
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
          void jobsApi.logs(jobId, { tail: LOG_TAIL }).then((l) => {
            if (alive) {
              mergeTailWindow(l)
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
        if (newStatus.status === 'failed') {
          const failed = firstFailedStep()
          if (failed != null && failed >= 0) {
            targetStep.value = failed
            router.replace({ query: { step: failed } })
            if (!expandedSteps.value.has(failed)) {
              expandedSteps.value.add(failed)
              nextTick(() => {
                if (!alive) return
                initSingleTerminal(failed)
                fitTerminalHeight(failed)
              })
            }
          }
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
  // Pending steps have no logs yet — unless a prepare-stage error was attached.
  if (getStepStatus(stepOrder) === 'pending' && !stepLogs.value.has(stepOrder)) return
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

const rerunning = ref(false)

const canRerunJob = computed(() => {
  const s = job.value?.status
  return s === 'success' || s === 'failed' || s === 'cancelled' || s === 'timeout'
})

async function handleRerun() {
  if (rerunning.value) return
  const attempt = (job.value?.run_attempt ?? 0) + 1
  if (!(await dialog.confirm(
    `Re-run this job from its frozen snapshot? Same run number, attempt ${attempt}, inputs carried over. A protected environment will require approval again.`,
    { title: 'Re-run job', confirmLabel: 'Re-run' },
  ))) return
  rerunning.value = true
  try {
    const resp = await jobsApi.rerun(jobId)
    managedWs?.close()
    router.push(`/jobs/${resp.job_id}`)
  } catch (e) { console.error(e) }
  finally { rerunning.value = false }
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

function formatDurationMs(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  const hours = Math.floor(total / 3600)
  const minutes = Math.floor((total % 3600) / 60)
  const seconds = total % 60
  if (hours > 0) return `${hours}h ${minutes}m`
  if (minutes > 0) return `${minutes}m ${seconds}s`
  return `${seconds}s`
}

const jobDuration = computed(() => {
  const j = job.value
  if (!j?.start_time) return '—'
  const start = parseUtc(j.start_time)
  const end = j.end_time ? parseUtc(j.end_time) : Date.now()
  return formatDurationMs(end - start)
})

const JOB_STATUS_META: Record<string, { label: string; tile: string; labelClass: string }> = {
  success: { label: 'Succeeded', tile: 'bg-emerald-500/10 text-emerald-600', labelClass: 'text-emerald-600' },
  failed: { label: 'Failed', tile: 'bg-red-500/10 text-red-600', labelClass: 'text-red-600' },
  running: { label: 'Running', tile: 'bg-amber-500/10 text-amber-600', labelClass: 'text-amber-600' },
  waiting_approval: { label: 'Waiting approval', tile: 'bg-amber-500/10 text-amber-700', labelClass: 'text-amber-700' },
  queued: { label: 'Queued', tile: 'bg-purple-500/10 text-purple-600', labelClass: 'text-purple-600' },
  cancelled: { label: 'Cancelled', tile: 'bg-slate-500/10 text-slate-500', labelClass: 'text-slate-500' },
  pending: { label: 'Pending', tile: 'bg-muted text-muted-foreground', labelClass: 'text-muted-foreground' },
}

const jobStatusMeta = computed(
  () =>
    JOB_STATUS_META[job.value?.status || 'pending'] || {
      label: job.value?.status || 'Unknown',
      tile: 'bg-muted text-muted-foreground',
      labelClass: 'text-foreground',
    },
)

const shortRunnerId = computed(() =>
  job.value?.runner_id ? `${String(job.value.runner_id).slice(0, 8)}…` : '—',
)

const runnerDisplay = computed(() => {
  const labels = job.value?.snapshot?.runner_labels || []
  if (labels.length) return labels.join(' · ')
  return shortRunnerId.value
})

const profileDisplay = computed(() => {
  const j = job.value
  if (!j?.resource_profile) return 'default'
  return j.resource_slots
    ? `${j.resource_profile} · ${j.resource_slots} slot${j.resource_slots > 1 ? 's' : ''}`
    : j.resource_profile
})

const stepStatusList = computed<string[]>(() =>
  (job.value?.snapshot?.tasks || []).map((task) =>
    getStepStatus(Number(task.step_order)),
  ),
)

const stepCounts = computed(() => {
  const counts: Record<string, number> = {
    success: 0,
    failed: 0,
    running: 0,
    cancelled: 0,
    pending: 0,
  }
  for (const status of stepStatusList.value) {
    counts[status] = (counts[status] || 0) + 1
  }
  return counts
})

const failedStepInfo = computed(() => {
  const j = job.value
  if (!j || j.status !== 'failed') return null
  const tasks = (j.snapshot?.tasks || []) as SnapshotTask[]
  const order = firstFailedStep()
  if (order != null && order >= 0) {
    const task = tasks.find((item) => Number(item.step_order) === order)
    return {
      title: `Failed at step ${String(order + 1).padStart(2, '0')}${task ? ` · ${task.name}` : ''}`,
      detail: 'Open the failed step to inspect the error output.',
      jump: Boolean(task),
    }
  }
  if (order === -1) {
    return {
      title: 'Failed during job preparation',
      detail: 'The Runner failed before the first step; the error is attached to the job log.',
      jump: tasks.length > 0,
    }
  }
  return {
    title: 'Job failed',
    detail: 'Inspect the logs below for the error output.',
    jump: tasks.length > 0,
  }
})

const inputEntries = computed(() =>
  Object.entries(job.value?.snapshot?.inputs || {}),
)

const concurrencySummary = computed(() => {
  const c = concurrencyInfo.value
  if (!c) return '—'
  if (!c.configured) return 'not configured'
  return `${c.group} · ${concurrencyModeLabel(c.mode)}`
})

function progressClass(status: string): string {
  switch (status) {
    case 'success': return 'bg-emerald-500'
    case 'failed': return 'bg-red-500'
    case 'running': return 'bg-amber-400'
    case 'cancelled': return 'bg-slate-400'
    default: return 'bg-border'
  }
}

function stepDotClass(status: string): string {
  switch (status) {
    case 'success': return 'bg-emerald-500'
    case 'failed': return 'bg-red-500'
    case 'running': return 'bg-amber-400'
    case 'cancelled': return 'bg-slate-400'
    default: return 'bg-border'
  }
}

function stepWrapperClass(task: SnapshotTask): string {
  const order = Number(task.step_order)
  const status = getStepStatus(order)
  if (status === 'pending') return 'border-border/40 bg-muted/20 opacity-50'
  if (status === 'failed') return 'border-red-400/30 bg-red-500/5 shadow-xs hover:border-red-400/40'
  if (targetStep.value === order) return 'border-primary/30 bg-primary/5 shadow-xs'
  if (expandedSteps.value.has(order)) return 'border-border bg-muted/30'
  return 'border-border/40 hover:border-border'
}

function stepDurationLabel(stepOrder: number): string {
  const found = job.value?.step_statuses?.find(
    (status: any) => Number(status.step_order) === stepOrder,
  )
  if (!found?.started_at) return '—'
  const start = parseUtc(found.started_at)
  const end = found.finished_at ? parseUtc(found.finished_at) : Date.now()
  return formatDurationMs(end - start)
}

function snapshotStepSubLabel(task: SnapshotTask): string {
  const uses = task.marketplace?.uses_ref || ''
  if (uses) return task.host_id ? `${uses} · ${task.host_id}` : uses
  const engine = execLabel(task.exec_type)
  if (task.host_id) return `${engine} · ${task.host_id}`
  const command = String(task.command_template || '').trim().split('\n')[0]
  return command ? `${engine} · ${command}` : engine
}

function marketplaceInputList(
  task: SnapshotTask,
): Array<{ name: string; description?: string | null; required?: boolean | null }> {
  const inputs = task.marketplace?.inputs
  if (Array.isArray(inputs)) return inputs as any
  if (inputs && typeof inputs === 'object') {
    return Object.entries(inputs).map(([name, value]) => ({
      name,
      ...(value && typeof value === 'object' ? value : {}),
    }))
  }
  return []
}

function ensureStepOpen(stepOrder: number) {
  const order = Number(stepOrder)
  if (getStepStatus(order) === 'pending' && !stepLogs.value.has(order)) return
  targetStep.value = order
  router.replace({ query: { step: order } })
  if (!expandedSteps.value.has(order)) {
    expandedSteps.value.add(order)
    nextTick(() => {
      initSingleTerminal(order)
      fitTerminalHeight(order)
    })
  }
  nextTick(() => {
    document.getElementById('step-' + order)?.scrollIntoView({
      behavior: 'smooth',
      block: 'center',
    })
  })
}

function expandAllSteps() {
  const tasks = (job.value?.snapshot?.tasks || []) as SnapshotTask[]
  for (const task of tasks) {
    const order = Number(task.step_order)
    if (getStepStatus(order) === 'pending' && !stepLogs.value.has(order)) continue
    expandedSteps.value.add(order)
  }
  nextTick(() => {
    for (const task of tasks) {
      const order = Number(task.step_order)
      if (!expandedSteps.value.has(order)) continue
      initSingleTerminal(order)
      fitTerminalHeight(order)
    }
  })
}

function collapseAllSteps() {
  for (const order of Array.from(expandedSteps.value)) {
    expandedSteps.value.delete(order)
    const index = stepTerminals.value.findIndex((item) => item.stepOrder === order)
    if (index !== -1) {
      stepTerminals.value[index].term.dispose()
      stepTerminals.value.splice(index, 1)
    }
  }
}

function jumpToFailedStep() {
  const order = firstFailedStep()
  const target = order != null && order >= 0 ? Number(order) : resolveGlobalChunkStep()
  if (target == null || target < 0) return
  ensureStepOpen(target)
}

async function copyStepLogs(stepOrder: number) {
  const text = (stepLogs.value.get(stepOrder) || [])
    .map((chunk) => chunk.content)
    .join('')
  if (!text) return
  const ok = await copyText(text)
  if (!ok) return
  copiedLogStep.value = stepOrder
  window.setTimeout(() => {
    if (copiedLogStep.value === stepOrder) copiedLogStep.value = null
  }, 1500)
}

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
          <UiButton v-if="canRerunJob" variant="outline" size="sm" class="text-xs px-3 py-1.5" :disabled="rerunning" @click="handleRerun">
            <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': rerunning }" /> Re-run
          </UiButton>
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
      <div class="space-y-4">
        <!-- Run summary -->
        <section class="overflow-hidden rounded-2xl border border-border/50 bg-card shadow-xs">
          <div class="flex flex-wrap items-start gap-5 px-5 py-4">
            <div class="flex min-w-0 items-center gap-3">
              <span class="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl" :class="jobStatusMeta.tile">
                <CheckCircle2 v-if="job.status === 'success'" class="h-5 w-5" />
                <XCircle v-else-if="job.status === 'failed' || job.status === 'cancelled'" class="h-5 w-5" />
                <Loader2 v-else-if="job.status === 'running'" class="h-5 w-5 animate-spin" />
                <Clock v-else class="h-5 w-5" />
              </span>
              <div class="min-w-0">
                <div class="text-[9px] font-semibold uppercase tracking-[0.12em] text-muted-foreground/60">Run status</div>
                <div class="text-[20px] font-semibold leading-tight tracking-tight" :class="jobStatusMeta.labelClass">{{ jobStatusMeta.label }}</div>
              </div>
            </div>
            <div class="border-l border-border/50 pl-5">
              <div class="text-[9px] font-semibold uppercase tracking-[0.12em] text-muted-foreground/60">Duration</div>
              <div class="text-[19px] font-semibold leading-tight tracking-tight">{{ jobDuration }}</div>
              <div class="mt-0.5 font-mono text-[9.5px] text-muted-foreground">
                {{ formatLocalDateTime(job.start_time) }} → {{ formatLocalDateTime(job.end_time) }}
              </div>
            </div>
            <div class="grid min-w-[24rem] flex-1 grid-cols-2 gap-x-6 gap-y-3 sm:grid-cols-4">
              <div class="min-w-0">
                <div class="text-[9px] font-semibold uppercase tracking-[0.12em] text-muted-foreground/60">Runner</div>
                <div class="mt-0.5 break-words font-mono text-[10.5px] text-foreground/80">{{ runnerDisplay }}</div>
              </div>
              <div class="min-w-0">
                <div class="text-[9px] font-semibold uppercase tracking-[0.12em] text-muted-foreground/60">Triggered by</div>
                <div class="mt-0.5 text-[11.5px] text-foreground/80">{{ job.trigger_by || '—' }}</div>
              </div>
              <div class="min-w-0">
                <div class="text-[9px] font-semibold uppercase tracking-[0.12em] text-muted-foreground/60">Environment</div>
                <div class="mt-0.5">
                  <span v-if="job.snapshot?.environment" class="mono-chip text-[10px]">{{ job.snapshot.environment }}</span>
                  <span v-else class="text-[11.5px] text-muted-foreground">—</span>
                </div>
              </div>
              <div class="min-w-0">
                <div class="text-[9px] font-semibold uppercase tracking-[0.12em] text-muted-foreground/60">Profile</div>
                <div class="mt-0.5 break-words font-mono text-[10.5px] text-foreground/80">{{ profileDisplay }}</div>
              </div>
            </div>
          </div>

          <div v-if="stepStatusList.length" class="flex flex-wrap items-center gap-3 px-5 pb-3">
            <div class="flex min-w-[12rem] flex-1 gap-1" aria-hidden="true">
              <span
                v-for="(status, index) in stepStatusList"
                :key="index"
                class="h-1.5 flex-1 rounded-full"
                :class="progressClass(status)"
              ></span>
            </div>
            <div class="flex flex-wrap items-center gap-3 text-[10px] text-muted-foreground">
              <span class="inline-flex items-center gap-1 text-emerald-600">
                <CheckCircle2 class="h-3 w-3" />{{ stepCounts.success }} passed
              </span>
              <span v-if="stepCounts.failed" class="inline-flex items-center gap-1 text-red-600">
                <XCircle class="h-3 w-3" />{{ stepCounts.failed }} failed
              </span>
              <span v-if="stepCounts.running" class="inline-flex items-center gap-1 text-amber-600">
                <Loader2 class="h-3 w-3 animate-spin" />{{ stepCounts.running }} running
              </span>
              <span>{{ stepStatusList.length }} steps</span>
            </div>
          </div>

          <div
            v-if="failedStepInfo"
            class="flex flex-wrap items-center gap-2 border-t border-red-500/15 bg-red-500/5 px-5 py-2.5 text-red-600"
          >
            <AlertTriangle class="h-3.5 w-3.5 shrink-0" />
            <div class="flex min-w-0 flex-wrap items-baseline gap-x-2 gap-y-0.5 text-[11px]">
              <strong class="font-semibold">{{ failedStepInfo.title }}</strong>
              <span class="text-red-600/80">{{ failedStepInfo.detail }}</span>
            </div>
            <button
              v-if="failedStepInfo.jump"
              type="button"
              class="ml-auto inline-flex items-center gap-1 text-[10.5px] font-medium hover:underline"
              @click="jumpToFailedStep"
            >
              View failed step <ChevronRight class="h-3 w-3" />
            </button>
          </div>
        </section>

        <!-- Run notices -->
        <div
          v-if="job.verbose_logs"
          class="flex items-start gap-2 rounded-xl border border-sky-500/30 bg-sky-500/10 px-4 py-3 text-sm"
        >
          <Info class="mt-0.5 h-4 w-4 shrink-0 text-sky-600" />
          <span class="text-sky-800">
            <span class="font-semibold">Verbose mode</span> — this run includes extra per-step detail
            (duration, exit code, full step config, stage progress). Secrets remain redacted.
          </span>
        </div>

        <div
          v-if="job.debug_unredact"
          class="flex items-start gap-2 rounded-xl border border-amber-500/40 bg-amber-500/10 px-4 py-3 text-sm"
        >
          <AlertTriangle class="mt-0.5 h-4 w-4 shrink-0 text-amber-600" />
          <span class="text-amber-800">
            <span class="font-semibold">Debug mode</span> — this run's logs were not redacted (plaintext
            secret values may appear). They are visible to platform admins only and are
            <span class="font-semibold">automatically purged 24 hours</span> after the job was created.
          </span>
        </div>

        <div
          v-if="job.status === 'waiting_approval'"
          class="rounded-xl border border-amber-500/40 bg-amber-500/10 px-4 py-3 text-sm"
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
                <template
                  v-if="!auth.isPlatformAdmin && job.snapshot?.protection?.forbid_self_approve !== false && job.trigger_user_id === auth.userId"
                >
                  (self-approve is forbidden)
                </template>.
              </p>
            </div>
          </div>
        </div>

        <div
          v-if="job.status === 'queued'"
          class="flex items-start gap-2 rounded-xl border border-purple-500/30 bg-purple-500/10 px-4 py-3 text-sm"
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

        <!-- Secondary information: Run details (3/4) + Approval history (1/4) -->
        <div
          class="grid grid-cols-1 gap-4"
          :class="showApprovalSidebar ? 'lg:grid-cols-[minmax(0,3fr)_minmax(0,1fr)]' : ''"
        >
          <section class="overflow-hidden rounded-2xl border border-border/50 bg-card shadow-xs">
            <div class="flex flex-wrap items-center justify-between gap-2 border-b border-border/30 px-4 py-3">
              <div class="text-[12.5px] font-semibold tracking-tight">Run details</div>
              <span class="text-[10px] text-muted-foreground/60">Metadata, policy, inputs, environment</span>
            </div>

            <details class="job-details border-b border-border/30 last:border-b-0">
              <summary class="flex cursor-pointer list-none items-center gap-2.5 px-4 py-2.5 hover:bg-muted/40 [&::-webkit-details-marker]:hidden">
                <span class="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
                  <Server class="h-3.5 w-3.5" />
                </span>
                <span class="min-w-0 flex-1">
                  <span class="block text-[11px] font-medium">Job info</span>
                  <span class="block truncate text-[9.5px] text-muted-foreground">
                    runner {{ shortRunnerId }} · retry {{ job.retry_times }} · {{ profileDisplay }}
                  </span>
                </span>
                <ChevronRight class="job-details-chevron h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              </summary>
              <div class="border-t border-border/30 bg-muted/20 px-4 py-3">
                <div class="grid grid-cols-1 gap-x-5 gap-y-2 sm:grid-cols-2">
                  <div class="min-w-0">
                    <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Runner ID</div>
                    <div class="mt-0.5 break-all font-mono text-[10.5px] text-foreground/80">{{ job.runner_id || '—' }}</div>
                  </div>
                  <div class="min-w-0">
                    <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Triggered by</div>
                    <div class="mt-0.5 text-[10.5px] text-foreground/80">{{ job.trigger_by || '—' }}</div>
                  </div>
                  <div class="min-w-0">
                    <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Retry</div>
                    <div class="mt-0.5 font-mono text-[10.5px] text-foreground/80">{{ job.retry_times }}</div>
                  </div>
                  <div class="min-w-0">
                    <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Profile</div>
                    <div class="mt-0.5 font-mono text-[10.5px] text-foreground/80">{{ profileDisplay }}</div>
                  </div>
                  <div v-if="job.dispatch_reason" class="min-w-0">
                    <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Dispatch</div>
                    <div class="mt-0.5 text-[10.5px] text-amber-600">{{ job.dispatch_reason }}</div>
                  </div>
                  <div v-if="job.resource_spec" class="min-w-0">
                    <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Resource envelope</div>
                    <div class="mt-0.5 break-all font-mono text-[10.5px] text-foreground/80">
                      {{ job.resource_spec?.memory || '—' }} · {{ job.resource_spec?.cpu || '—' }} cpu · pids {{ job.resource_spec?.pids ?? '—' }}
                    </div>
                  </div>
                  <div class="min-w-0">
                    <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Created</div>
                    <div class="mt-0.5 font-mono text-[10.5px] text-foreground/80">{{ formatLocalDateTime(job.created_at) }}</div>
                  </div>
                  <div class="min-w-0">
                    <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Started</div>
                    <div class="mt-0.5 font-mono text-[10.5px] text-foreground/80">{{ formatLocalDateTime(job.start_time) }}</div>
                  </div>
                  <div class="min-w-0">
                    <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Ended</div>
                    <div class="mt-0.5 font-mono text-[10.5px] text-foreground/80">{{ formatLocalDateTime(job.end_time) }}</div>
                  </div>
                </div>
              </div>
            </details>

            <details class="job-details border-b border-border/30 last:border-b-0">
              <summary class="flex cursor-pointer list-none items-center gap-2.5 px-4 py-2.5 hover:bg-muted/40 [&::-webkit-details-marker]:hidden">
                <span class="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
                  <List class="h-3.5 w-3.5" />
                </span>
                <span class="min-w-0 flex-1">
                  <span class="block text-[11px] font-medium">Concurrency</span>
                  <span class="block truncate text-[9.5px] text-muted-foreground">{{ concurrencySummary }}</span>
                </span>
                <ChevronRight class="job-details-chevron h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              </summary>
              <div class="border-t border-border/30 bg-muted/20 px-4 py-3">
                <template v-if="concurrencyInfo">
                  <div class="flex flex-wrap gap-1.5">
                    <template v-if="concurrencyInfo.configured">
                      <span class="mono-chip text-[10px]"><span class="text-muted-foreground/60">group </span>{{ concurrencyInfo.group }}</span>
                      <span class="rounded-md border border-border/50 bg-card px-1.5 py-0.5 text-[10px] font-medium">{{ concurrencyModeLabel(concurrencyInfo.mode) }}</span>
                      <template v-if="concurrencyInfo.mode === 'queue'">
                        <span class="mono-chip text-[10px]"><span class="text-muted-foreground/60">max queue </span>{{ concurrencyInfo.maxQueue ?? '—' }}</span>
                        <span class="mono-chip text-[10px]"><span class="text-muted-foreground/60">timeout </span>{{ formatQueueTimeout(concurrencyInfo.queueTimeoutSec) }}</span>
                      </template>
                    </template>
                    <span v-else class="rounded-md border border-border/50 bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">not configured</span>
                  </div>
                  <p class="mt-2 text-[10px] leading-relaxed text-muted-foreground">
                    {{ concurrencyModeHint(concurrencyInfo.configured, concurrencyInfo.mode) }}
                  </p>
                </template>
              </div>
            </details>

            <details class="job-details border-b border-border/30 last:border-b-0">
              <summary class="flex cursor-pointer list-none items-center gap-2.5 px-4 py-2.5 hover:bg-muted/40 [&::-webkit-details-marker]:hidden">
                <span class="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
                  <Sliders class="h-3.5 w-3.5" />
                </span>
                <span class="min-w-0 flex-1">
                  <span class="block text-[11px] font-medium">Trigger inputs</span>
                  <span class="block truncate text-[9.5px] text-muted-foreground">
                    {{ inputEntries.length }} input{{ inputEntries.length === 1 ? '' : 's' }}
                  </span>
                </span>
                <ChevronRight class="job-details-chevron h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              </summary>
              <div class="border-t border-border/30 bg-muted/20 px-4 py-3">
                <div v-if="inputEntries.length" class="flex flex-wrap gap-1.5">
                  <span
                    v-for="[key, value] in inputEntries"
                    :key="key"
                    class="inline-flex items-center gap-1.5 rounded-md border px-2 py-1 font-mono text-[10px]"
                    :class="isMaskedInput(String(key)) ? 'border-violet-500/20 bg-violet-500/5 text-violet-700' : 'border-border/50 bg-card text-foreground/75'"
                  >
                    <span class="text-muted-foreground/70">{{ key }}</span>
                    <span v-if="isMaskedInput(String(key))" class="font-sans text-[8.5px] font-medium uppercase tracking-wide text-violet-600">masking</span>
                    <span class="font-medium">{{ displayInputValue(String(key), value) }}</span>
                  </span>
                </div>
                <p v-else class="text-[10.5px] text-muted-foreground">No inputs recorded for this run.</p>
              </div>
            </details>

            <details class="job-details border-b border-border/30 last:border-b-0">
              <summary class="flex cursor-pointer list-none items-center gap-2.5 px-4 py-2.5 hover:bg-muted/40 [&::-webkit-details-marker]:hidden">
                <span class="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
                  <Lock class="h-3.5 w-3.5" />
                </span>
                <span class="min-w-0 flex-1">
                  <span class="block text-[11px] font-medium">Environment &amp; secrets</span>
                  <span class="block truncate text-[9.5px] text-muted-foreground">
                    {{ job.snapshot?.environment || 'no environment' }} · {{ (job.snapshot?.env || []).length }} entries
                  </span>
                </span>
                <ChevronRight class="job-details-chevron h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              </summary>
              <div class="border-t border-border/30 bg-muted/20 px-4 py-3">
                <div class="flex flex-wrap items-center gap-2">
                  <span class="text-[10px] text-muted-foreground">Environment</span>
                  <span v-if="job.snapshot?.environment" class="mono-chip text-[10px]">{{ job.snapshot.environment }}</span>
                  <span v-else class="text-[10.5px] text-muted-foreground">—</span>
                </div>
                <div v-if="(job.snapshot?.env || []).length" class="mt-2 flex flex-wrap gap-1.5">
                  <span
                    v-for="entry in job.snapshot?.env || []"
                    :key="entry.name"
                    class="inline-flex items-center gap-1 rounded-md border px-1.5 py-0.5 font-mono text-[10px]"
                    :class="entry.is_secret ? 'border-amber-400/20 bg-amber-400/5 text-amber-700' : 'border-border/50 bg-card text-foreground/70'"
                  >
                    <Lock v-if="entry.is_secret" class="h-2.5 w-2.5" />
                    {{ entry.name }}
                  </span>
                </div>
                <p v-else class="mt-2 text-[10.5px] text-muted-foreground">No environment or env entries recorded.</p>
                <p class="mt-2 text-[10px] leading-relaxed text-muted-foreground">
                  Environment Variable / Secret defaults are overlaid for this run; pipeline <code class="font-mono">env:</code> still wins.
                </p>
              </div>
            </details>

            <details class="job-details border-b border-border/30 last:border-b-0">
              <summary class="flex cursor-pointer list-none items-center gap-2.5 px-4 py-2.5 hover:bg-muted/40 [&::-webkit-details-marker]:hidden">
                <span class="flex h-6 w-6 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
                  <FileCode2 class="h-3.5 w-3.5" />
                </span>
                <span class="min-w-0 flex-1">
                  <span class="block text-[11px] font-medium">Snapshot</span>
                  <span class="block truncate text-[9.5px] text-muted-foreground">
                    captured {{ job.snapshot?.triggered_at ? job.snapshot.triggered_at.slice(0, 19).replace('T', ' ') : '—' }}
                    · {{ (job.snapshot?.tasks || []).length }} tasks
                  </span>
                </span>
                <ChevronRight class="job-details-chevron h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              </summary>
              <div class="border-t border-border/30 bg-muted/20 px-4 py-3">
                <div class="grid grid-cols-1 gap-x-5 gap-y-2 sm:grid-cols-2">
                  <div class="min-w-0">
                    <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Captured</div>
                    <div class="mt-0.5 font-mono text-[10.5px] text-foreground/80">{{ formatLocalDateTime(job.snapshot?.triggered_at) }}</div>
                  </div>
                  <div class="min-w-0">
                    <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Tasks</div>
                    <div class="mt-0.5 font-mono text-[10.5px] text-foreground/80">{{ (job.snapshot?.tasks || []).length }}</div>
                  </div>
                  <div class="min-w-0">
                    <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Runner labels</div>
                    <div class="mt-0.5 break-words font-mono text-[10.5px] text-foreground/80">{{ (job.snapshot?.runner_labels || []).join(' · ') || '—' }}</div>
                  </div>
                  <div class="min-w-0">
                    <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Pipeline</div>
                    <router-link
                      :to="'/pipelines/' + job.pipeline_id + '/definition'"
                      class="mt-0.5 inline-block text-[10.5px] text-primary underline underline-offset-2 hover:text-primary/80"
                    >{{ job.pipeline_name || job.pipeline_id }}</router-link>
                  </div>
                </div>
              </div>
            </details>
          </section>

          <aside
            v-if="showApprovalSidebar"
            class="overflow-hidden rounded-2xl border border-border/50 bg-card shadow-xs"
          >
            <div class="flex items-center gap-2 border-b border-border/30 px-4 py-3">
              <ScrollText class="h-3.5 w-3.5 text-muted-foreground" />
              <span class="text-[12.5px] font-semibold">Approval history</span>
              <span
                v-if="approvalList.length > 0"
                class="ml-auto rounded-md bg-muted px-1.5 py-0.5 font-mono text-[9.5px] text-muted-foreground"
              >{{ approvalList.length }}</span>
            </div>
            <p class="px-4 pt-2 text-[10px] leading-relaxed text-muted-foreground">
              Append-only trail (approve / reject / cancel / timeout).
            </p>
            <div v-if="approvalList.length === 0" class="px-4 py-8 text-center text-[11px] text-muted-foreground">
              No approval events yet.
            </div>
            <ul v-else class="divide-y divide-border/20">
              <li v-for="item in approvalList" :key="item.id" class="px-4 py-3">
                <div class="flex flex-wrap items-center gap-2">
                  <span
                    class="inline-flex items-center rounded-md border px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide"
                    :class="approvalActionClass(item.action)"
                  >{{ item.action }}</span>
                  <span class="truncate text-[11px] font-medium">{{ item.username || item.user_id || '—' }}</span>
                  <span class="ml-auto whitespace-nowrap font-mono text-[9px] text-muted-foreground">
                    {{ item.created_at ? formatLocalDateTime(item.created_at) : '—' }}
                  </span>
                </div>
                <p v-if="item.comment" class="mt-1.5 whitespace-pre-wrap break-words text-[10px] text-muted-foreground">{{ item.comment }}</p>
              </li>
            </ul>
          </aside>
        </div>

        <!-- Steps (full width) -->
        <section
          v-if="job.snapshot && job.snapshot.tasks && job.snapshot.tasks.length > 0"
          class="overflow-hidden rounded-2xl border border-border/50 bg-card shadow-xs"
        >
          <div class="flex flex-wrap items-center justify-between gap-3 border-b border-border/30 px-5 py-3.5">
            <div class="flex flex-wrap items-center gap-2.5">
              <h2 class="text-[11px] font-semibold uppercase tracking-[0.1em] text-muted-foreground">Steps</h2>
              <span class="rounded-full border border-border/50 bg-muted px-1.5 py-0.5 font-mono text-[9.5px] text-muted-foreground">{{ job.snapshot.tasks.length }}</span>
              <span class="rounded-md bg-muted px-1.5 py-0.5 font-mono text-[9.5px] text-muted-foreground">Snapshot</span>
              <span v-if="job.snapshot.triggered_at" class="font-mono text-[10px] text-muted-foreground/60">
                {{ job.snapshot.triggered_at.slice(0, 19).replace('T', ' ') }}
              </span>
            </div>
            <div class="flex flex-wrap items-center gap-3">
              <router-link
                :to="'/pipelines/' + job.pipeline_id + '/definition'"
                class="text-[10.5px] text-muted-foreground underline underline-offset-2 hover:text-foreground"
              >View pipeline definition</router-link>
              <button type="button" class="text-[10.5px] text-muted-foreground transition-colors hover:text-foreground" @click="expandAllSteps">Expand all</button>
              <button type="button" class="text-[10.5px] text-muted-foreground transition-colors hover:text-foreground" @click="collapseAllSteps">Collapse all</button>
              <button
                v-if="logs.length > 0 && !earlierExhausted"
                type="button"
                class="text-[10.5px] text-muted-foreground transition-colors hover:text-foreground disabled:opacity-50"
                :disabled="loadingEarlier"
                @click="loadEarlierLogs"
              >{{ loadingEarlier ? 'Loading…' : 'Load earlier logs' }}</button>
            </div>
          </div>

          <div class="flex flex-wrap gap-1 border-b border-border/30 bg-muted/30 px-5 py-2">
            <button
              v-for="task in job.snapshot.tasks"
              :key="task.step_order"
              type="button"
              class="inline-flex items-center gap-1.5 rounded-md px-2 py-1 font-mono text-[9.5px] transition-colors"
              :class="targetStep === task.step_order ? 'border border-border bg-card text-foreground shadow-xs' : 'text-muted-foreground hover:bg-card/60 hover:text-foreground'"
              :disabled="getStepStatus(task.step_order) === 'pending' && !stepLogs.has(task.step_order)"
              @click="ensureStepOpen(task.step_order)"
            >
              <span class="h-1.5 w-1.5 rounded-full" :class="stepDotClass(getStepStatus(task.step_order))"></span>
              {{ String(task.step_order + 1).padStart(2, '0') }}
              <span class="hidden font-sans text-[10px] sm:inline">{{ task.name }}</span>
            </button>
          </div>

          <div class="space-y-1.5 px-4 py-3">
            <div
              v-for="task in job.snapshot.tasks"
              :id="'step-' + task.step_order"
              :key="task.step_order"
              class="overflow-hidden rounded-xl border transition-colors"
              :class="stepWrapperClass(task)"
            >
              <button
                type="button"
                class="flex w-full items-center gap-2.5 px-3 py-3 text-left"
                :disabled="getStepStatus(task.step_order) === 'pending' && !stepLogs.has(task.step_order)"
                @click="toggleStep(task.step_order)"
              >
                <span class="shrink-0">
                  <CheckCircle2 v-if="getStepStatus(task.step_order) === 'success'" class="h-4 w-4 text-emerald-600" />
                  <XCircle v-else-if="getStepStatus(task.step_order) === 'failed'" class="h-4 w-4 text-red-600" />
                  <Loader2 v-else-if="getStepStatus(task.step_order) === 'running'" class="h-4 w-4 animate-spin text-amber-600" />
                  <XCircle v-else-if="getStepStatus(task.step_order) === 'cancelled'" class="h-4 w-4 text-slate-400" />
                  <Clock v-else class="h-4 w-4 text-muted-foreground/40" />
                </span>
                <span class="shrink-0 font-mono text-[9px] text-muted-foreground/60">
                  {{ String(task.step_order + 1).padStart(2, '0') }}
                </span>
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-[12.5px] font-semibold text-foreground">{{ task.name }}</span>
                  <span class="mt-0.5 flex flex-wrap items-center gap-1 font-mono text-[9px] text-muted-foreground">
                    <span class="truncate">{{ snapshotStepSubLabel(task) }}</span>
                    <span
                      v-if="task.marketplace?.reviewed != null"
                      :class="task.marketplace.reviewed ? 'text-emerald-600' : 'text-amber-600'"
                    >· {{ task.marketplace.reviewed ? 'reviewed' : 'unreviewed' }}</span>
                    <span v-if="task.fail_strategy === 'continue'" class="text-amber-600">· continue-on-error</span>
                    <span
                      v-if="task.capabilities?.length"
                      class="inline-flex items-center gap-0.5 rounded bg-amber-500/10 px-1 text-[8.5px] text-amber-700"
                      :title="'Requests (not grants): ' + task.capabilities.join(', ') + ' — a Runner admin must authorize locally.'"
                    >
                      <AlertTriangle class="h-2.5 w-2.5" />{{ task.capabilities.join(', ') }}
                    </span>
                  </span>
                </span>
                <span class="shrink-0 font-mono text-[9.5px] text-muted-foreground">{{ stepDurationLabel(task.step_order) }}</span>
                <ChevronRight
                  class="job-details-chevron h-3.5 w-3.5 shrink-0 text-muted-foreground"
                  :class="{ 'rotate-90': expandedSteps.has(task.step_order) }"
                />
              </button>

              <div v-if="expandedSteps.has(task.step_order)" class="px-3 pb-3">
                <div class="overflow-hidden rounded-lg border border-border bg-[#0d1117]">
                  <div class="flex h-8 items-center gap-2 border-b border-white/10 bg-[#161b22] px-3 select-none">
                    <span class="truncate text-[10px] font-medium text-slate-200">{{ task.name }}</span>
                    <span class="rounded-sm border border-white/15 px-1 font-mono text-[8.5px] text-slate-400">{{ execLabel(task.exec_type) }}</span>
                    <div class="flex-1" />
                    <span
                      class="inline-flex items-center gap-1 text-[9.5px]"
                      :class="getStepStatus(task.step_order) === 'failed' ? 'text-red-400' : 'text-slate-400'"
                    >
                      <span class="h-1.5 w-1.5 rounded-full" :class="terminalTitleStatus(getStepStatus(task.step_order)).dot"></span>
                      {{ terminalTitleStatus(getStepStatus(task.step_order)).label }}
                    </span>
                    <button
                      type="button"
                      class="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[9.5px] text-slate-400 transition-colors hover:bg-white/10 hover:text-slate-200"
                      @click.stop="copyStepLogs(task.step_order)"
                    >
                      <Check v-if="copiedLogStep === task.step_order" class="h-3 w-3" />
                      <Copy v-else class="h-3 w-3" />
                      {{ copiedLogStep === task.step_order ? 'Copied' : 'Copy' }}
                    </button>
                  </div>
                  <div class="relative w-full bg-[#0d1117] px-3 py-3">
                    <div :id="'terminal-el-' + task.step_order" class="w-full" />
                    <div
                      v-if="(stepLogs.get(task.step_order) || []).length === 0"
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

                <details class="job-details mt-2">
                  <summary class="inline-flex cursor-pointer list-none items-center gap-1 text-[10px] text-muted-foreground hover:text-foreground [&::-webkit-details-marker]:hidden">
                    Step details <ChevronRight class="job-details-chevron h-3 w-3" />
                  </summary>
                  <div class="mt-2 rounded-lg border border-border/50 bg-card px-3 py-2.5">
                    <div class="grid grid-cols-1 gap-x-5 gap-y-2 sm:grid-cols-2">
                      <div class="min-w-0">
                        <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Engine</div>
                        <div class="mt-0.5 font-mono text-[10.5px] text-foreground/80">{{ task.exec_type }}</div>
                      </div>
                      <div v-if="task.host_id" class="min-w-0">
                        <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Host</div>
                        <div class="mt-0.5 break-all font-mono text-[10.5px] text-foreground/80">{{ task.host_id }}</div>
                      </div>
                      <div v-if="task.work_dir" class="min-w-0">
                        <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Work dir</div>
                        <div class="mt-0.5 break-all font-mono text-[10.5px] text-foreground/80">{{ task.work_dir }}</div>
                      </div>
                      <div v-if="task.timeout_seconds > 0" class="min-w-0">
                        <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Timeout</div>
                        <div class="mt-0.5 font-mono text-[10.5px] text-foreground/80">{{ formatDurationMs(task.timeout_seconds * 1000) }}</div>
                      </div>
                      <div v-if="task.retry_count > 0" class="min-w-0">
                        <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Retry</div>
                        <div class="mt-0.5 font-mono text-[10.5px] text-foreground/80">{{ task.retry_count }}</div>
                      </div>
                      <div class="min-w-0">
                        <div class="text-[8px] font-semibold uppercase tracking-[0.08em] text-muted-foreground/60">Failure</div>
                        <div class="mt-0.5 font-mono text-[10.5px] text-foreground/80">{{ task.fail_strategy === 'continue' ? 'continue-on-error' : 'stop' }}</div>
                      </div>
                    </div>

                    <div
                      v-if="task.marketplace"
                      class="mt-2 rounded-md border border-border/50 bg-muted/20 px-2.5 py-2 font-mono text-[9.5px] leading-relaxed text-muted-foreground/80"
                    >
                      <div class="flex flex-wrap items-center gap-x-2 gap-y-1">
                        <span class="font-semibold text-foreground/70">{{ task.marketplace.uses_ref }}</span>
                        <span v-if="task.marketplace.resolved_version">v{{ task.marketplace.resolved_version }}</span>
                        <span :class="task.marketplace.reviewed ? 'text-emerald-600' : 'text-amber-600'">{{ task.marketplace.reviewed ? 'reviewed' : 'unreviewed' }}</span>
                      </div>
                      <div v-if="task.marketplace.content_sha256" class="mt-1 break-all">sha256:{{ task.marketplace.content_sha256 }}</div>
                      <div v-if="task.marketplace.source" class="break-all">source: {{ task.marketplace.source }}</div>
                      <div v-if="task.marketplace.requires_buz" class="text-muted-foreground/60">Requires Buz {{ task.marketplace.requires_buz }}</div>
                      <div v-if="marketplaceInputList(task).length" class="mt-1 space-y-0.5">
                        <div class="text-[8.5px] uppercase tracking-wide text-muted-foreground/60">Declared plugin inputs</div>
                        <div v-for="input in marketplaceInputList(task)" :key="input.name" class="break-all">
                          {{ input.name }}<span v-if="input.required"> (required)</span><span v-if="input.description"> — {{ input.description }}</span>
                        </div>
                      </div>
                    </div>

                    <div
                      v-if="task.capabilities?.length"
                      class="mt-2 rounded-md border border-red-500/20 bg-red-500/5 px-2.5 py-1.5 text-[10px] leading-relaxed text-red-600"
                    >
                      <span class="font-semibold">Requests (not grants):</span>
                      {{ task.capabilities.join(', ') }} — a Runner administrator must authorize it locally.
                    </div>

                    <div
                      v-if="task.command_template"
                      class="mt-2 whitespace-pre-wrap break-words rounded-md border border-border/50 bg-muted/20 px-2.5 py-2 font-mono text-[10px] leading-relaxed text-foreground/80"
                    >{{ task.command_template }}</div>
                  </div>
                </details>
              </div>
            </div>
          </div>
        </section>

        <div
          v-else
          class="rounded-2xl border border-border/50 bg-card/60 p-12 text-center text-sm text-muted-foreground"
        >
          No snapshot data
        </div>
      </div>
    </template>

    <div v-else class="rounded-2xl border border-border/50 bg-card/60 backdrop-blur-xs p-12 text-center">
      <p class="text-sm text-muted-foreground">Job not found</p>
    </div>
  </div>
</template>

<style scoped>
.job-details-chevron {
  transition: transform 0.15s ease;
}
details[open] > summary .job-details-chevron {
  transform: rotate(90deg);
}
</style>
