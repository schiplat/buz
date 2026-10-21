<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch, nextTick } from 'vue'
import { useRoute, useRouter, RouterLink } from 'vue-router'
import UiButton from '@/components/ui/UiButton.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import { ArrowLeft, Play, Loader2, Plus, Trash2, CheckCircle2, XCircle, Clock, Pencil, X, ChevronDown, ChevronRight, Copy, Check, Zap, Server, Link, Settings, Braces, Sliders, List, RotateCw, Webhook, MoreHorizontal, Eye, CircleHelp, Bell, AlertTriangle, AlertCircle, Snowflake } from '@lucide/vue'
import { cn } from '@/lib/utils'
import {
  pipelinesApi,
  type PipelineInputDef,
  type TriggerEvent,
  type GitHookConfig,
  type YamlParsedData,
} from '@/api/pipelines'
import {
  collectSecretNamesFromYaml,
  concurrencyFromYaml,
  envEntriesFromRecord,
  envEntriesFromRows,
  parseDurationToSeconds,
  yamlStepsToDefTasks,
} from '@/utils/pipelineDefinitionView'
import { findMissingRequiredInputs, normalizeInputDefsForForm } from '@/utils/pipelineInputs'
import { formatConfigRenderSummary } from '@/utils/configForm'
import { secretsApi } from '@/api/secrets'
import { variablesApi } from '@/api/variables'
import { teamsApi, type Team } from '@/api/teams'
import { jobsApi, type Job, type LogChunk } from '@/api/jobs'
import { environmentsApi, type Environment } from '@/api/environments'
import { parseUtc, formatLocalShort } from '@/utils/time'
import { copyText } from '@/utils/copyText'
import { parseApiError, type FormattedApiError } from '@/utils/error'
import PipelineTopologyGraph from '@/components/flow/PipelineTopologyGraph.vue'
import BuildHistoryCard from '@/components/flow/BuildHistoryCard.vue'
import StepLogPanel from '@/components/flow/StepLogPanel.vue'
import { createManagedWebSocket, type ManagedWebSocket } from '@/utils/ws'
import { navLog } from '@/utils/navigation'
import { useAuthStore } from '@/stores/auth'
import PipelineYamlEditorDialog from '@/components/pipeline/PipelineYamlEditorDialog.vue'
import { useAppDialog } from '@/composables/useAppDialog'

const route = useRoute()
const router = useRouter()
const auth = useAuthStore()
const dialog = useAppDialog()
const pipelineId = computed(() => route.params.id as string)
const yamlEditorRef = ref<InstanceType<typeof PipelineYamlEditorDialog> | null>(null)

const pipeline = ref<any>(null)

// When route.params.id changes (navigating between pipelines), reload data
watch(pipelineId, async () => {
  const id = pipelineId.value
  if (!id) return
  if (refreshTimer) { clearInterval(refreshTimer); refreshTimer = null }
  stopHistoryLogPoll()
  historyLogs.value.clear()
  pipelineWs?.close()
  pipelineWs = null
  jobs.value = []
  pipeline.value = null
  defYamlAuthority.value = null
  defYamlParsed.value = null
  defYamlErrors.value = []
  defYamlFilename.value = ''
  defYamlName.value = ''
  await loadData()
  startPipelineWebSocket()
  if (activeTab.value === 'definition') {
    await loadYamlDefinition()
  }
})
const jobs = ref<Job[]>([])
const loading = ref(true)
const triggering = ref(false)
/** Shown inside the trigger dialog when POST /trigger fails (e.g. 422 YAML validation). */
const triggerError = ref<FormattedApiError | null>(null)
const activeTab = computed(() => (route.params.tab as string) || 'stage')
let refreshTimer: ReturnType<typeof setInterval> | null = null
let pipelineWs: ManagedWebSocket | null = null
let alive = true

const showTriggerDialog = ref(false)
type InputPair = {
  key: string
  value: string
  required?: boolean
  masking?: boolean
  /** Declared constraint: string / number / boolean / host / choice. */
  type?: string
  pattern?: string
  options?: string[]
}
const inputPairs = ref<InputPair[]>([])
/** Field names flagged by the last validation error — highlighted red in the dialog. */
const errorFields = computed<Set<string>>(() => {
  const set = new Set<string>()
  for (const item of triggerError.value?.items || []) {
    if (item.field) set.add(item.field)
  }
  return set
})
/** Ad-hoc params when pipeline has no input_defs — collapsed by default. */
const showAdhocParams = ref(false)
const triggerDebug = ref(false)
const triggerVerbose = ref(false)
const triggerEnvironment = ref('')
const environments = ref<Environment[]>([])
const environmentLoadFailed = ref(false)
const editAvailableSecrets = ref<{ id: string; name: string; owner_group: string; kind?: string }[]>([])
const editAvailableVariables = ref<{ id: string; name: string; value: string }[]>([])
const editAvailableTeams = ref<Team[]>([])

const selectedTriggerEnv = computed(() =>
  environments.value.find((e) => e.slug === triggerEnvironment.value) || null,
)
const declaredTriggerEnvironments = computed(() => {
  const declared = new Set<string>(pipeline.value?.environments || [])
  return environments.value.filter((environment) => declared.has(environment.slug))
})

async function loadEnvironments() {
  try {
    environments.value = await environmentsApi.list()
    environmentLoadFailed.value = false
  } catch (e) {
    console.error(e)
    environments.value = []
    environmentLoadFailed.value = true
  }
}

// Pagination
const historyPage = ref(1)
const historyPageSize = 10

// ── Trigger events + Git hook ──
const triggerEvents = ref<TriggerEvent[]>([])
const triggerEventsTotal = ref(0)
const triggerEventsPage = ref(1)
const triggerEventsPageSize = 20
const triggerEventsLoading = ref(false)
const triggerFilterSource = ref('')
const triggerFilterDecision = ref('')
const gitHooks = ref<GitHookConfig[]>([])
const gitHookSaving = ref(false)
/** Plaintext secrets shown once after create/rotate, keyed by hook id. */
const gitHookSecretsOnce = ref<Record<string, string>>({})
const gitHookCopied = ref<{ id: string; kind: 'url' | 'secret' } | null>(null)
const newHookProvider = ref<'github' | 'gitee'>('gitee')
const newHookLabel = ref('')
const newHookBranchFilter = ref('')
const newHookEnvironment = ref('')
const newHookAvailableEnvironments = computed(() => {
  const declared = new Set<string>(pipeline.value?.environments || [])
  return environments.value.filter((environment) => declared.has(environment.slug))
})

async function loadTriggerEvents() {
  if (!pipelineId.value) return
  triggerEventsLoading.value = true
  try {
    const page = await pipelinesApi.listTriggerEvents(pipelineId.value, {
      source: triggerFilterSource.value || undefined,
      decision: triggerFilterDecision.value || undefined,
      page: triggerEventsPage.value,
      page_size: triggerEventsPageSize,
    })
    triggerEvents.value = page.items || []
    triggerEventsTotal.value = page.total || 0
  } catch (e) {
    console.error(e)
  } finally {
    triggerEventsLoading.value = false
  }
}

async function loadGitHooks() {
  if (!pipelineId.value) return
  try {
    gitHooks.value = await pipelinesApi.listGitHooks(pipelineId.value)
  } catch (e) {
    console.error(e)
  }
}

async function addGitHook() {
  if (!pipelineId.value) return
  gitHookSaving.value = true
  try {
    const h = await pipelinesApi.createGitHook(pipelineId.value, {
      provider: newHookProvider.value,
      label: newHookLabel.value.trim(),
      branch_filter: newHookBranchFilter.value,
      environment: newHookEnvironment.value,
      enabled: true,
    })
    if (h.secret) {
      gitHookSecretsOnce.value = { ...gitHookSecretsOnce.value, [h.id]: h.secret }
    }
    newHookLabel.value = ''
    newHookBranchFilter.value = ''
    newHookEnvironment.value = ''
    await loadGitHooks()
  } catch (e: any) {
    await dialog.alert(e?.response?.data?.message || e?.message || 'Failed to create git hook')
  } finally {
    gitHookSaving.value = false
  }
}

async function removeGitHook(hook: GitHookConfig) {
  if (!pipelineId.value) return
  if (!(await dialog.confirm(
    `Delete this Git webhook${hook.label ? ` (“${hook.label}”)` : ''}? Create a new one to change settings.`,
    { title: 'Delete webhook', confirmLabel: 'Delete', danger: true },
  ))) return
  try {
    await pipelinesApi.deleteGitHook(pipelineId.value, hook.id)
    const next = { ...gitHookSecretsOnce.value }
    delete next[hook.id]
    gitHookSecretsOnce.value = next
    await loadGitHooks()
  } catch (e: any) {
    await dialog.alert(e?.response?.data?.message || e?.message || 'Failed to delete git hook')
  }
}

const windowOrigin = computed(() => (typeof window !== 'undefined' ? window.location.origin : ''))

function copyGitHookField(hook: GitHookConfig, kind: 'url' | 'secret') {
  const text =
    kind === 'url'
      ? `${windowOrigin.value}${hook.url_path || ''}`
      : (gitHookSecretsOnce.value[hook.id] || '')
  if (!text) return
  void copyText(text).then((ok) => {
    if (!ok) return
    gitHookCopied.value = { id: hook.id, kind }
    setTimeout(() => { gitHookCopied.value = null }, 2000)
  })
}

function shortSha(sha: string) {
  return sha ? sha.slice(0, 7) : '—'
}

function decisionClass(d: string) {
  if (d === 'triggered') return 'text-emerald-600'
  if (d === 'skipped') return 'text-amber-600'
  if (d === 'rejected' || d === 'failed') return 'text-red-500'
  return 'text-muted-foreground'
}

async function openEditDialog() {
  if (!pipeline.value) return
  await yamlEditorRef.value?.openEdit({
    id: pipeline.value.id,
    name: pipeline.value.name,
    source: pipeline.value.source || 'form',
    yaml_path: pipeline.value.yaml_path || null,
  })
}

async function onYamlSaved() {
  await loadData()
  await loadYamlDefinition()
}

// ──────────────────────────────

/** File-first Definition payload for `source=yaml` pipelines. */
const defYamlLoading = ref(false)
const defYamlAuthority = ref<'file' | 'unavailable' | null>(null)
const defYamlFilename = ref('')
const defYamlName = ref('')
const defYamlErrors = ref<string[]>([])
const defYamlParsed = ref<YamlParsedData | null>(null)

async function loadYamlDefinition() {
  if (!pipeline.value || pipeline.value.source !== 'yaml') {
    defYamlAuthority.value = null
    defYamlParsed.value = null
    defYamlErrors.value = []
    defYamlFilename.value = ''
    defYamlName.value = ''
    return
  }
  defYamlLoading.value = true
  defYamlErrors.value = []
  try {
    const src = await pipelinesApi.getYamlSource(pipelineId.value)
    defYamlFilename.value = src.filename
    const validation = await pipelinesApi.validateYaml(src.yaml)
    const errs = (validation.errors || []).map(
      (e) => `${e.field}: ${e.message}`,
    )
    // Prefer parsed tree even when referential checks fail (missing secret, etc.)
    // so Definition can still show notify / steps from the file.
    if (!validation.parsed) {
      defYamlAuthority.value = 'unavailable'
      defYamlParsed.value = null
      defYamlName.value = ''
      defYamlErrors.value = errs.length
        ? errs
        : ['YAML could not be parsed']
      return
    }
    defYamlAuthority.value = 'file'
    defYamlParsed.value = validation.parsed
    defYamlName.value = validation.name || pipeline.value.name
    defYamlErrors.value = validation.valid ? [] : errs
  } catch (e: any) {
    defYamlAuthority.value = 'unavailable'
    defYamlParsed.value = null
    defYamlName.value = ''
    defYamlFilename.value = pipeline.value?.yaml_path || ''
    defYamlErrors.value = [
      e?.response?.data?.message || e?.message || 'Failed to load YAML file',
    ]
  } finally {
    defYamlLoading.value = false
  }
}

const hasInputDefs = computed(() => {
  // Trigger dialog still uses DB projection (runtime).
  const defs: PipelineInputDef[] | undefined = pipeline.value?.input_defs
  return Array.isArray(defs) && defs.length > 0
})

const successCount = computed(() => jobs.value.filter(j => j.status === 'success').length)
const failedCount = computed(() => jobs.value.filter(j => j.status === 'failed').length)
const runningCount = computed(() => jobs.value.filter(j => j.status === 'running').length)
const hasRunningJobs = computed(() => runningCount.value > 0 || jobs.value.some(j => j.status === 'pending'))

const stageViewRuns = computed(() => jobs.value.slice(0, Math.min(jobs.value.length, 8)))
const latestRun = computed(() => jobs.value[0] ?? null)

/** Stage View: focused step in the topology (null = none). */
const stageFocusStep = ref<number | null>(null)

const stageFocusTask = computed(() => {
  const idx = stageFocusStep.value
  if (idx == null) return null
  const tasks = (pipeline.value?.tasks || []) as { name?: string; exec_type?: string; step_order?: number }[]
  return tasks[idx] ?? null
})

const stageFocusStatus = computed(() => {
  const idx = stageFocusStep.value
  if (idx == null) return 'pending'
  const ss = latestRun.value?.step_statuses || null
  if (!ss) return 'pending'
  const found = (ss as any[]).find((s: any) => Number(s.step_order) === idx)
  return found?.status || 'pending'
})

watch(stageFocusStep, async (idx) => {
  if (idx == null) return
  const job = latestRun.value
  if (!job) return
  await loadHistoryLogs(job.id)
})
const hasYamlSource = computed(
  () => pipeline.value?.source === 'yaml' || Boolean(pipeline.value?.yaml_source),
)

const yamlSync = computed(() => pipeline.value?.yaml_sync ?? null)

/** Definition-page banner: prefer live file-load errors, else yaml_sync status.
 *  Only shown when action is needed — idle in-sync state uses the header chip. */
const yamlSyncBanner = computed(() => {
  if (hasYamlSource.value && defYamlAuthority.value === 'unavailable') {
    return {
      tone: 'destructive' as const,
      title: 'Cannot load definition from file',
      detail: 'On-disk YAML is missing or invalid. Definition is not shown from the DB projection.',
      errors: defYamlErrors.value,
    }
  }
  if (hasYamlSource.value && defYamlAuthority.value === 'file' && defYamlErrors.value.length) {
    return {
      tone: 'warning' as const,
      title: 'YAML validation issues',
      detail: 'Fix these before trigger. The file is still shown below.',
      errors: defYamlErrors.value,
    }
  }
  const s = yamlSync.value
  if (!s || s.status === 'not_file_backed' || s.status === 'in_sync') return null
  if (s.status === 'validation_failed') {
    return {
      tone: 'destructive' as const,
      title: 'YAML validation failed',
      detail: 'Trigger is blocked until the on-disk file is fixed.',
      errors: s.errors || [],
    }
  }
  if (s.status === 'missing_file') {
    return {
      tone: 'destructive' as const,
      title: 'YAML file missing',
      detail: `Expected ${s.filename || 'file'} under Manager yaml_scan_dir.`,
      errors: [] as string[],
    }
  }
  if (s.status === 'stale') {
    return {
      tone: 'warning' as const,
      title: 'File newer than last sync',
      detail: 'Content below is from disk. The next trigger will refresh the DB projection.',
      errors: [] as string[],
    }
  }
  return null
})

/**
 * Definition view model: file authority when source=yaml and parse ok;
 * otherwise DB projection (legacy / non-yaml). Null when yaml unavailable.
 */
const definitionView = computed(() => {
  const p = pipeline.value
  if (!p) return null

  if (p.source === 'yaml') {
    if (defYamlAuthority.value !== 'file' || !defYamlParsed.value) return null
    const parsed = defYamlParsed.value
    const tc = parsed.trigger_config
    const runner = parsed.runner
    return {
      authority: 'file' as const,
      name: defYamlName.value || p.name,
      description: parsed.description || '',
      id: p.id,
      trigger_type: parsed.trigger_type || 'manual',
      webhook_path: tc?.webhook_path || '',
      webhook_environment: tc?.webhook_environment || '',
      cron_expr: tc?.cron_expr || '',
      cron_environment: tc?.cron_environment || '',
      runner_labels: runner?.labels || [],
      timeout_seconds: parseDurationToSeconds(runner?.timeout) || p.timeout_seconds || 3600,
      environments: parsed.environments || [],
      variableNames: parsed.variables || [],
      secretNames: collectSecretNamesFromYaml(parsed),
      envEntries: envEntriesFromRecord(parsed.env),
      notify: Array.isArray(parsed.notify) ? parsed.notify : [],
      team_id: p.team_id,
      source: 'yaml',
      input_defs: (tc?.inputs || []).map((i) => ({
        name: i.name,
        description: i.description || undefined,
        required: !!i.required,
        default: i.default || undefined,
        masking: !!i.masking,
        type: i.type || undefined,
        pattern: i.pattern || undefined,
        options: i.options || undefined,
      })),
      tasks: yamlStepsToDefTasks(parsed.steps || []).map((task, index) => ({
        ...task,
        capabilities: p.tasks?.[index]?.capabilities || [],
        marketplace: p.tasks?.[index]?.marketplace || null,
      })),
      concurrency: concurrencyFromYaml(parsed),
      filename: defYamlFilename.value,
    }
  }

  // Legacy / non-yaml: DB projection
  const group = (p.concurrency_group || '').trim()
  const concurrency = !group
    ? {
        configured: false as const,
        group: null as string | null,
        mode: null as string | null,
        maxQueue: null as number | null,
        queueTimeoutSec: null as number | null,
      }
    : {
        configured: true as const,
        group,
        mode: (p.concurrency_mode || '').trim() || 'discard-new',
        maxQueue: p.concurrency_max_queue ?? 3,
        queueTimeoutSec: p.concurrency_queue_timeout_seconds ?? 1800,
      }
  return {
    authority: 'db' as const,
    name: p.name,
    description: p.description || '',
    id: p.id,
    trigger_type: p.trigger_type || 'manual',
    webhook_path: p.webhook_path || '',
    webhook_environment: p.webhook_environment || '',
    cron_expr: p.cron_expr || '',
    cron_environment: p.cron_environment || '',
    runner_labels: p.runner_labels || [],
    timeout_seconds: p.timeout_seconds || 3600,
    environments: p.environments || [],
    variableNames: boundVariableNames.value,
    secretNames: boundSecretNames.value,
    envEntries: envEntriesFromRows(p.env),
    notify: [] as { type: string; webhook: string; on?: string[] | null }[],
    team_id: p.team_id,
    source: p.source || 'form',
    input_defs: Array.isArray(p.input_defs) ? p.input_defs : [],
    tasks: p.tasks || [],
    concurrency,
    filename: '',
  }
})

const showConcurrencyHelp = ref(false)

/** Live pipeline concurrency settings for Definition tab (always shown). */
const pipelineConcurrency = computed(() => definitionView.value?.concurrency ?? null)

/** Notify channels from YAML (Definition tab). */
const pipelineNotify = computed(() => definitionView.value?.notify ?? [])

const defHasInputDefs = computed(
  () => (definitionView.value?.input_defs?.length || 0) > 0,
)

function formatConcurrencyTimeout(sec: number | null): string {
  if (sec == null || sec <= 0) return '—'
  if (sec % 3600 === 0) return `${sec / 3600}h`
  if (sec % 60 === 0) return `${sec / 60}m`
  return `${sec}s`
}

function typeHint(t: string): string {
  switch (t) {
    case 'string':
      return 'Free-form text (default type)'
    case 'number':
      return 'Must be an integer'
    case 'boolean':
      return 'Must be true or false'
    case 'host':
      return 'Must reference a registered Host name'
    case 'choice':
      return 'Must be one of the declared options'
    default:
      return 'Declared value constraint'
  }
}

function pipelineConcurrencyHint(configured: boolean, mode: string | null): string {
  if (!configured) {
    return 'No concurrency group — triggers are not serialized. Multiple jobs may run in parallel.'
  }
  switch (mode) {
    case 'queue':
      return 'FIFO: new triggers wait while a job is pending/running, up to max-queue depth.'
    case 'discard-old':
      return 'Keep the newest waiting job; older queued jobs are cancelled. Running jobs are never cancelled.'
    case 'discard-new':
      return 'Reject new triggers with HTTP 409 while any job is pending, running, or queued.'
    default:
      return ''
  }
}
const historyTotalPages = computed(() => Math.ceil(jobs.value.length / historyPageSize))
const historyPagedJobs = computed(() => {
  const start = (historyPage.value - 1) * historyPageSize
  return jobs.value.slice(start, start + historyPageSize)
})

// ── History log loading (lazy, on card expand) ──
const historyLogs = ref<Map<string, LogChunk[]>>(new Map())

async function loadHistoryLogs(jobId: string) {
  if (!historyLogs.value.has(jobId)) {
    const job = jobs.value.find(j => j.id === jobId)
    // Debug-job logs are platform-admin-only; never attempt the fetch for
    // non-admins (the Manager would reject it with 403 anyway).
    if (job?.debug_unredact && !auth.isPlatformAdmin) return
    try {
      const logs = await jobsApi.logs(jobId)
      historyLogs.value.set(jobId, logs)
    } catch { /* ignore */ }
  }
  // Keep already-expanded live jobs streaming while the user is watching.
  startHistoryLogPoll()
}

function getHistoryLogs(jobId: string): LogChunk[] {
  return historyLogs.value.get(jobId) || []
}

// ── Live history log polling (step terminals on the Build History tab) ──
// Pipeline-level WS only pushes job *status*; log chunks are per-job. Poll the
// logs endpoint for live jobs the user has already expanded so their step
// terminals stream instead of freezing at first load.
let historyLogPoll: ReturnType<typeof setInterval> | null = null
let historyLogPollBusy = false

function liveLoadedJobIds(): string[] {
  return jobs.value
    .filter((j) => j.status === 'running' || j.status === 'pending')
    .map((j) => j.id)
    .filter((id) => historyLogs.value.has(id))
}

function startHistoryLogPoll() {
  if (historyLogPoll) return
  historyLogPoll = setInterval(async () => {
    if (!alive || historyLogPollBusy) return
    if (liveLoadedJobIds().length === 0) return
    historyLogPollBusy = true
    try {
      for (const id of liveLoadedJobIds()) {
        const job = jobs.value.find((j) => j.id === id)
        if (job?.debug_unredact && !auth.isPlatformAdmin) continue
        try {
          const logs = await jobsApi.logs(id, { timeout: 5000 })
          if (alive) historyLogs.value.set(id, logs)
        } catch { /* ignore */ }
      }
    } finally {
      historyLogPollBusy = false
    }
  }, 3000)
}

function stopHistoryLogPoll() {
  if (historyLogPoll) { clearInterval(historyLogPoll); historyLogPoll = null }
  historyLogPollBusy = false
}

function runNum(job: Job, paged: Job[]) {
  // Show the global run number (1-based from oldest)
  const idx = jobs.value.findIndex(j => j.id === job.id)
  return idx >= 0 ? jobs.value.length - idx : 0
}

function historyGo(page: number) {
  historyPage.value = Math.max(1, Math.min(page, historyTotalPages.value))
}

/** Deep-link target from Trigger events (`?job=`). */
const historyFocusJobId = computed(() => {
  const q = route.query.job
  return typeof q === 'string' && q ? q : ''
})

function runNumberForJobId(jobId: string): number | null {
  const idx = jobs.value.findIndex(j => j.id === jobId)
  return idx >= 0 ? jobs.value.length - idx : null
}

async function focusHistoryJob(jobId: string) {
  const idx = jobs.value.findIndex(j => j.id === jobId)
  if (idx < 0) return
  historyPage.value = Math.floor(idx / historyPageSize) + 1
  await nextTick()
  document.getElementById(`hist-job-${jobId}`)?.scrollIntoView({ behavior: 'smooth', block: 'nearest' })
}

watch(
  [activeTab, historyFocusJobId, jobs],
  async () => {
    if (activeTab.value !== 'history' || !historyFocusJobId.value) return
    await focusHistoryJob(historyFocusJobId.value)
  },
  { flush: 'post' },
)

/** Returns the raw status string (e.g. 'success', 'running'), not a CSS class. */
function getStepStatus(stepIndex: number, stepStatuses: any[] | null): string {
  if (!stepStatuses) return 'pending'
  const found = stepStatuses.find((s: any) => Number(s.step_order) === stepIndex)
  return found?.status || 'pending'
}

/** Prefer YAML host names for SSH display; fall back to resolved host_id. */
function sshHostDisplay(step: { host_names?: string | null; host_id?: string | null }): string {
  const names = (step.host_names || '').toString().trim()
  if (names) return names
  return (step.host_id || '').toString().trim()
}

function fmtDuration(start: string | null, end: string | null) {
  if (!start) return '—'
  const a = parseUtc(start)
  const b = end ? parseUtc(end) : Date.now()
  const s = Math.max(0, Math.floor((b - a) / 1000))
  if (s < 60) return `${s}s`
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`
}

/** Per-step duration from step_statuses entry (started_at → finished_at). */
function fmtStepDuration(stepIndex: number, stepStatuses: any[] | null): string {
  if (!stepStatuses) return '—'
  const found = stepStatuses.find((s: any) => Number(s.step_order) === stepIndex)
  if (!found?.started_at) return '—'
  const a = parseUtc(found.started_at)
  const b = found.finished_at ? parseUtc(found.finished_at) : Date.now()
  const s = Math.max(0, Math.floor((b - a) / 1000))
  if (s < 60) return `${s}s`
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`
}

function fmtTime(ts: string | null | undefined) {
  return formatLocalShort(ts, '—')
}

function timeAgo(ts: string | null | undefined) {
  if (!ts) return '—'
  const diff = Math.max(0, Math.floor((Date.now() - parseUtc(ts)) / 1000))
  if (diff < 60) return `${diff}s ago`
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`
  return `${Math.floor(diff / 86400)}d ago`
}

// ── Sidebar mini timeline helpers ──

interface BuildDay { label: string; jobs: Job[] }

const timelineBuilds = computed<BuildDay[]>(() => {
  const now = new Date()
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime()
  const yesterday = today - 86400_000

  const groups: Record<string, Job[]> = { Today: [], Yesterday: [], Earlier: [] }
  for (const j of jobs.value.slice(0, 12)) {
    const ts = parseUtc(j.created_at)
    if (ts >= today) {
      groups.Today.push(j)
    } else if (ts >= yesterday) {
      groups.Yesterday.push(j)
    } else {
      groups.Earlier.push(j)
    }
  }
  const result: BuildDay[] = []
  for (const label of ['Today', 'Yesterday', 'Earlier']) {
    if (groups[label].length > 0) result.push({ label, jobs: groups[label] })
  }
  return result
})

function runningDuration(j: Job): string {
  if (!j.start_time) return 'just started'
  const s = Math.max(0, Math.floor((Date.now() - parseUtc(j.start_time)) / 1000))
  if (s < 60) return `${s}s`
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`
}

const showMenuForJob = ref<string | null>(null)

function toggleJobMenu(jobId: string, e: Event) {
  e.stopPropagation()
  showMenuForJob.value = showMenuForJob.value === jobId ? null : jobId
}

function closeJobMenu() { showMenuForJob.value = null }

async function cancelSidebarJob(jobId: string) {
  try {
    await jobsApi.cancel(jobId)
    showMenuForJob.value = null
  } catch (e) { console.error(e) }
}

function viewJobLogs(jobId: string) {
  showMenuForJob.value = null
  router.push(`/jobs/${jobId}`)
}

function navToJob(jobId: string) {
  router.push(`/jobs/${jobId}`)
}

// ── Click-outside to close job menu ──
function handleSidebarClick(e: MouseEvent) {
  const target = e.target as HTMLElement
  if (!target.closest('.sidebar-job-menu')) closeJobMenu()
}

onMounted(async () => {
  await loadData()
  startPipelineWebSocket()
  await loadEditInfrastructureOptions()
  if (activeTab.value === 'definition') {
    await loadYamlDefinition()
  }
  if (activeTab.value === 'trigger-events') {
    await Promise.all([loadTriggerEvents(), loadGitHooks(), loadEnvironments()])
  }
})

watch(activeTab, async (tab) => {
  if (tab === 'definition') {
    await loadYamlDefinition()
  }
  if (tab === 'trigger-events') {
    await Promise.all([loadTriggerEvents(), loadGitHooks(), loadEnvironments()])
  }
})

watch([triggerFilterSource, triggerFilterDecision], () => {
  triggerEventsPage.value = 1
  if (activeTab.value === 'trigger-events') void loadTriggerEvents()
})

/** Refresh Secrets / Variables / Teams for definition labels. */
async function loadEditInfrastructureOptions() {
  const [secrets, variables, teams] = await Promise.allSettled([
    secretsApi.list(),
    variablesApi.list(),
    teamsApi.list(),
  ])
  if (secrets.status === 'fulfilled') editAvailableSecrets.value = secrets.value
  if (variables.status === 'fulfilled') editAvailableVariables.value = variables.value
  if (teams.status === 'fulfilled') editAvailableTeams.value = teams.value
}

function teamLabel(teamId: string | null | undefined): string {
  if (!teamId) return '—'
  const t = editAvailableTeams.value.find(x => x.id === teamId)
  return t ? `${t.name} (${t.slug})` : teamId
}

const boundVariableNames = computed(() => {
  if (!pipeline.value?.var_ids?.length) return []
  return pipeline.value.var_ids.map((vid: string) => {
    const v = editAvailableVariables.value.find(x => x.id === vid)
    return v ? v.name : vid
  })
})

const boundSecretNames = computed(() => {
  if (!pipeline.value?.secret_ids?.length) return []
  return pipeline.value.secret_ids.map((sid: string) => {
    const s = editAvailableSecrets.value.find(x => x.id === sid)
    return s ? s.name : sid
  })
})

onUnmounted(() => {
  alive = false
  if (refreshTimer) clearInterval(refreshTimer)
  stopHistoryLogPoll()
  pipelineWs?.close()
  pipelineWs = null
})

function startPipelineWebSocket() {
  pipelineWs?.close()
  const token = localStorage.getItem('buz_token') || ''
  const protocol = window.location.protocol === 'https:' ? 'wss' : 'ws'
  const wsUrl = `${protocol}://${window.location.host}/api/v1/pipelines/${pipelineId.value}/stream?token=${encodeURIComponent(token)}`
  pipelineWs = createManagedWebSocket({
    label: `pipeline:${pipelineId.value.slice(0, 8)}`,
    onOpen: () => navLog('[PipelineDetail] WS connected'),
    onMessage: (raw) => {
      if (!alive) return
      try {
        const msg = JSON.parse(raw)
        if (msg.type !== 'status' || !msg.data) return
        // Upsert job data into the jobs array
        const updated = msg.data
        const idx = jobs.value.findIndex((j: any) => j.id === updated.id)
        const merged = idx >= 0
          ? { ...jobs.value[idx], ...updated }
          : { ...updated, pipeline_id: pipelineId.value }
        if (idx >= 0) {
          jobs.value[idx] = merged
        } else {
          jobs.value.unshift(merged)
        }
      } catch { /* ignore unparseable messages */ }
    },
  })
  pipelineWs.connect(wsUrl)
}

function startRefreshTimer() {
  // No longer used — status updates arrive via WebSocket.
  // Kept as empty placeholder for backward compat.
}

async function loadData() {
  loading.value = true
  try {
    const [p, j] = await Promise.all([
      pipelinesApi.get(pipelineId.value),
      jobsApi.list(),
    ])
    pipeline.value = p
    const allJobs: any[] = Array.isArray(j) ? j : []
    jobs.value = allJobs.filter((job: any) => job.pipeline_id === pipelineId.value).map((job: any) => ({
      ...job,
      step_statuses: typeof job.step_statuses === 'string'
        ? JSON.parse(job.step_statuses)
        : (job.step_statuses || []),
      snapshot: typeof job.snapshot === 'string'
        ? JSON.parse(job.snapshot)
        : (job.snapshot || null),
    }))
  } catch (e) {
    console.error(e)
  } finally { loading.value = false }
}

async function handleToggleActive() {
  if (!pipeline.value) return
  if (pipeline.value.is_active) {
    const choice = await dialog.confirmChoice(
      `Disable pipeline "${pipeline.value.name}"? All trigger paths (manual, webhook, cron, git hook) will reject new runs.`,
      {
        title: 'Disable pipeline',
        confirmLabel: 'Disable',
        danger: true,
        checkboxLabel: 'Also cancel in-flight jobs (pending / queued / running / waiting approval)',
        checkboxDefault: true,
      },
    )
    if (!choice.ok) return
    try {
      const result = await pipelinesApi.disable(pipelineId.value, choice.checked)
      pipeline.value.is_active = false
      if (result.cancelled_count > 0) {
        await dialog.alert(
          `Pipeline disabled — cancelled ${result.cancelled_count} in-flight job(s).`,
          { title: 'Pipeline disabled' },
        )
        await loadData()
      }
    } catch (e) {
      console.error(e)
      await dialog.alert('Disable failed')
    }
  } else {
    const ok = await dialog.confirm(`Enable pipeline "${pipeline.value.name}"?`, {
      title: 'Enable pipeline',
      confirmLabel: 'Enable',
    })
    if (!ok) return
    try {
      const result = await pipelinesApi.enable(pipelineId.value)
      pipeline.value.is_active = result.is_active
    } catch (e) {
      console.error(e)
      await dialog.alert('Enable failed')
    }
  }
}

async function openTriggerDialog() {
  if (!pipeline.value) return
  await loadEnvironments()
  const defs: PipelineInputDef[] = pipeline.value.input_defs || []
  triggerDebug.value = false
  triggerVerbose.value = false
  triggerEnvironment.value = ''
  triggerError.value = null
  showAdhocParams.value = false
  // Always open the dialog so users can opt into Verbose / Debug before Run.
  inputPairs.value = defs.length > 0
    ? normalizeInputDefsForForm(defs).map((d) => ({
        key: d.name,
        value: d.default || '',
        required: d.required,
        masking: !!d.masking,
        type: d.type,
        pattern: d.pattern,
        options: d.options,
      }))
    : []
  showTriggerDialog.value = true
}

function navigateTab(tab: string) {
  // Clear deep-link query when switching tabs via the tab bar.
  router.push({ name: 'pipeline-detail', params: { id: pipelineId.value, tab }, query: {} })
}

function toggleAdhocParams() {
  showAdhocParams.value = !showAdhocParams.value
  if (showAdhocParams.value && inputPairs.value.length === 0) {
    inputPairs.value = [{ key: '', value: '' }]
  }
}

function addInputPair() { inputPairs.value.push({ key: '', value: '' }) }
function removeInputPair(idx: number) {
  inputPairs.value.splice(idx, 1)
}

async function handleTrigger() {
  if (!pipeline.value || triggering.value) return
  // Client-side required check: empty value == not filled. Blocks submission
  // before the request so the dialog can highlight the offending fields.
  const missing = findMissingRequiredInputs(inputPairs.value)
  if (missing.length > 0) {
    triggerError.value = {
      title: 'Invalid inputs',
      summary: 'Fill in the required fields highlighted below, then trigger again.',
      items: missing.map((name) => ({ field: name, message: 'required input is missing' })),
    }
    return
  }
  if (!triggerEnvironment.value || !declaredTriggerEnvironments.value.some((env) => env.slug === triggerEnvironment.value)) {
    triggerError.value = {
      title: 'Environment required',
      summary: 'Choose one of the environments declared by this pipeline.',
      items: [{ field: 'environment', message: 'select a declared environment' }],
    }
    return
  }
  triggering.value = true
  triggerError.value = null
  try {
    const inputs: Record<string, string> = {}
    for (const pair of inputPairs.value) {
      // Empty values are not sent so the backend merges declared defaults.
      if (pair.key.trim() && (pair.value ?? '').trim()) inputs[pair.key.trim()] = pair.value
    }
    const result = await pipelinesApi.trigger(
      pipelineId.value,
      inputs,
      triggerDebug.value,
      triggerVerbose.value,
      triggerEnvironment.value,
    )
    showTriggerDialog.value = false
    triggerDebug.value = false
    triggerVerbose.value = false
    triggerEnvironment.value = ''
    router.push(`/jobs/${result.job_id}`)
  } catch (e: any) {
    console.error(e)
    triggerError.value = parseApiError(e, 'Trigger failed')
  } finally {
    triggering.value = false
  }
}
</script>

<template>
  <div class="flex flex-col h-[calc(100vh-var(--header-h,3.5rem)-2rem)]">
    <!-- Top bar -->
    <div class="flex items-center justify-between shrink-0 mb-3">
      <div class="flex items-center gap-2.5 min-w-0">
        <button
          class="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground transition-colors shrink-0"
          @click="router.push('/pipelines')"
        >
          <ArrowLeft class="h-3.5 w-3.5" />
        </button>
        <div class="h-5 w-px bg-border/40 shrink-0" />
        <h1 class="text-base font-semibold tracking-tight truncate">{{ pipeline?.name || 'Loading…' }}</h1>
        <StatusBadge v-if="pipeline" :status="pipeline.is_active ? 'online' : 'offline'" />
      </div>
      <div class="flex items-center gap-2 shrink-0">
        <UiButton
          variant="ghost"
          size="sm"
          :disabled="!pipeline"
          :title="pipeline?.is_active ? 'Disable pipeline' : 'Enable pipeline'"
          @click="handleToggleActive"
        >
          <Snowflake
            class="h-4 w-4"
            :class="pipeline?.is_active ? 'text-cyan-500' : 'text-muted-foreground'"
          />
        </UiButton>
        <UiButton size="sm" @click="openTriggerDialog" :disabled="!pipeline || triggering || !pipeline.is_active" class="shrink-0">
          <Loader2 v-if="triggering" class="h-4 w-4 animate-spin" />
          <Play v-else class="h-4 w-4" />
          {{ triggering ? 'Running…' : 'Build Now' }}
        </UiButton>
      </div>
    </div>

    <!-- Trigger Dialog -->
    <Teleport to="body">
      <div v-if="showTriggerDialog" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-xs" @click="showTriggerDialog = false" />
        <div class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6 max-h-[90vh] overflow-y-auto">
          <h2 class="text-[15px] font-semibold tracking-tight">Trigger Pipeline</h2>
          <p v-if="hasInputDefs" class="mt-1.5 text-[12px] text-muted-foreground leading-relaxed">
            Keys available as <code class="mono-chip text-[11px]">$INPUT_&lt;KEY&gt;</code> in step commands.
            Masked fields are redacted in logs.
          </p>
          <p v-else class="mt-1.5 text-[12px] text-muted-foreground leading-relaxed">
            No declared parameters. Optionally enable Verbose/Debug, then Run.
          </p>

          <div
            v-if="triggerError"
            role="alert"
            class="mt-4 rounded-lg border border-destructive/40 bg-destructive/10 px-3 py-2.5 text-destructive"
          >
            <p class="text-[13px] font-semibold tracking-tight">{{ triggerError.title }}</p>
            <p v-if="triggerError.summary" class="mt-1 text-[11px] font-mono text-destructive/80 truncate" :title="triggerError.summary">
              {{ triggerError.summary }}
            </p>
            <ul class="mt-2.5 space-y-2 border-t border-destructive/20 pt-2.5">
              <li
                v-for="(item, i) in triggerError.items"
                :key="i"
                class="text-[12px] leading-relaxed"
              >
                <code
                  v-if="item.field"
                  class="mb-0.5 inline-block rounded-sm bg-destructive/15 px-1.5 py-0.5 font-mono text-[11px] text-destructive"
                >{{ item.field }}</code>
                <p :class="item.field ? 'mt-0.5 pl-0.5 opacity-95' : 'opacity-95'">{{ item.message }}</p>
              </li>
            </ul>
          </div>

          <!-- Declared inputs -->
          <div v-if="hasInputDefs" class="mt-5 space-y-2.5">
            <div
              v-for="(pair, idx) in inputPairs"
              :key="idx"
              class="rounded-lg border px-3 py-2.5 transition-colors"
              :class="errorFields.has(pair.key) ? 'border-destructive/60 bg-destructive/5' : 'border-border/40 bg-muted/20'"
            >
              <div class="mb-1.5 flex flex-wrap items-center gap-1.5">
                <span class="text-[13px] font-mono font-medium text-foreground">{{ pair.key }}</span>
                <span v-if="pair.required" class="text-red-500 text-[12px] leading-none" title="required">*</span>
                <span v-else class="text-[11px] text-muted-foreground/70">(optional)</span>
                <span
                  v-if="pair.masking"
                  class="rounded-sm px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide bg-violet-500/10 text-violet-600 border border-violet-500/20"
                >masking</span>
                <span
                  v-if="pair.type"
                  class="rounded-sm px-1.5 py-0.5 text-[10px] font-medium bg-sky-500/10 text-sky-600 border border-sky-500/20"
                >{{ pair.type }}</span>
              </div>

              <select
                v-if="pair.type === 'choice' || pair.type === 'boolean'"
                v-model="pair.value"
                class="w-full rounded-lg border bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50"
                :class="errorFields.has(pair.key) ? 'border-destructive' : 'border-border/60'"
                :disabled="triggering"
              >
                <option v-if="!pair.options?.length" value="" disabled>select…</option>
                <template v-if="pair.type === 'choice'">
                  <option v-for="opt in pair.options" :key="opt" :value="opt">{{ opt }}</option>
                </template>
                <template v-else>
                  <option value="true">true</option>
                  <option value="false">false</option>
                </template>
              </select>

              <textarea
                v-else-if="pair.type === 'list'"
                v-model="pair.value"
                rows="4"
                :placeholder="pair.required ? 'required — one item per line' : 'one item per line (leave empty for default)'"
                class="w-full rounded-lg border bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50 font-mono"
                :class="errorFields.has(pair.key) ? 'border-destructive' : 'border-border/60'"
                :disabled="triggering"
              />

              <input
                v-else
                v-model="pair.value"
                :type="pair.masking ? 'password' : pair.type === 'number' ? 'number' : 'text'"
                :placeholder="pair.required ? 'required' : 'enter value (leave empty for default)'"
                class="w-full rounded-lg border bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50"
                :class="errorFields.has(pair.key) ? 'border-destructive' : 'border-border/60'"
                :disabled="triggering"
                :autocomplete="pair.masking ? 'new-password' : 'off'"
              />
            </div>
            <p v-if="errorFields.size > 0" class="text-[12px] text-destructive">
              {{ errorFields.size }} invalid field(s) — fix them and trigger again.
            </p>
          </div>

          <!-- Ad-hoc params (no input_defs) — collapsed by default -->
          <div v-else class="mt-5">
            <button
              type="button"
              class="inline-flex items-center gap-1 text-[12px] font-medium text-muted-foreground hover:text-foreground transition-colors"
              :disabled="triggering"
              @click="toggleAdhocParams"
            >
              <ChevronDown v-if="showAdhocParams" class="h-3.5 w-3.5" />
              <ChevronRight v-else class="h-3.5 w-3.5" />
              Advanced: add parameters
            </button>
            <div v-if="showAdhocParams" class="mt-3 space-y-2">
              <p class="text-[11px] text-muted-foreground leading-relaxed">
                Ad-hoc keys become <code class="mono-chip text-[11px]">$INPUT_&lt;KEY&gt;</code> in step commands.
              </p>
              <div v-for="(pair, idx) in inputPairs" :key="idx" class="flex items-center gap-2">
                <input
                  v-model="pair.key"
                  placeholder="key"
                  class="flex-1 rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50"
                  :disabled="triggering"
                />
                <input
                  v-model="pair.value"
                  type="text"
                  placeholder="value"
                  class="flex-2 rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50"
                  :disabled="triggering"
                />
                <button
                  type="button"
                  class="shrink-0 rounded-lg p-2 text-muted-foreground hover:bg-muted hover:text-destructive transition-colors"
                  :disabled="triggering"
                  @click="removeInputPair(idx)"
                >
                  <Trash2 class="h-4 w-4" />
                </button>
              </div>
              <button
                type="button"
                class="flex items-center gap-1 text-[12px] font-medium text-primary hover:text-primary/80 transition-colors"
                :disabled="triggering"
                @click="addInputPair"
              >
                <Plus class="h-3.5 w-3.5" /> Add parameter
              </button>
            </div>
          </div>

          <div class="mt-5">
            <label class="mb-1.5 block text-[12px] font-medium">Environment <span class="text-red-500">*</span></label>
            <select
              v-model="triggerEnvironment"
              class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50"
              :disabled="triggering"
            >
              <option value="" disabled>Select a declared environment…</option>
              <option v-for="e in declaredTriggerEnvironments" :key="e.id" :value="e.slug">
                {{ e.name }} ({{ e.slug }}){{ e.protection_enabled ? ' — protected' : '' }}
              </option>
            </select>
            <p v-if="environmentLoadFailed" class="mt-1.5 text-[11px] text-destructive">
              Could not load registered environments. Retry after restoring Manager access.
            </p>
            <p v-else-if="declaredTriggerEnvironments.length === 0" class="mt-1.5 text-[11px] text-destructive">
              This pipeline declares no environments registered in the Manager.
            </p>
            <p
              v-else-if="selectedTriggerEnv?.protection_enabled"
              class="mt-1.5 rounded-lg border border-amber-500/30 bg-amber-500/10 px-2.5 py-1.5 text-[11px] leading-relaxed text-amber-800 dark:text-amber-300"
            >
              Requires approval after trigger (min role:
              {{ selectedTriggerEnv.min_trigger_role || 'developer' }}).
            </p>
            <p v-else class="mt-1.5 text-[11px] text-muted-foreground leading-relaxed">
              Overrides platform Variable defaults. Pipeline <code class="mono-chip text-[11px]">env:</code> still wins.
            </p>
          </div>

          <div class="mt-5 space-y-2.5">
            <label class="flex cursor-pointer items-center gap-2 text-[12px]">
              <input v-model="triggerVerbose" type="checkbox" :disabled="triggering" />
              <span class="font-medium">Verbose — more log detail</span>
              <span
                class="inline-flex text-muted-foreground/60 hover:text-muted-foreground"
                title="Stage markers, step config, duration / exit code; secrets stay redacted"
                @click.prevent
              >
                <CircleHelp class="h-3.5 w-3.5" />
              </span>
            </label>
            <label
              v-if="auth.isPlatformAdmin"
              class="flex cursor-pointer items-center gap-2 text-[12px]"
            >
              <input v-model="triggerDebug" type="checkbox" :disabled="triggering" />
              <span class="font-medium text-amber-700">Debug — show secrets (24h)</span>
              <span
                class="inline-flex text-amber-600/50 hover:text-amber-700"
                title="Unredacted logs; platform admin only; purged after 24h"
                @click.prevent
              >
                <CircleHelp class="h-3.5 w-3.5" />
              </span>
            </label>
          </div>

          <div class="mt-6 flex items-center justify-end gap-3 border-t border-border/30 pt-5">
            <UiButton variant="outline" size="sm" @click="showTriggerDialog = false">Cancel</UiButton>
            <UiButton size="sm" @click="handleTrigger" :disabled="triggering || !triggerEnvironment || declaredTriggerEnvironments.length === 0">
              <Loader2 v-if="triggering" class="mr-1.5 h-4 w-4 animate-spin" />
              <Play v-else class="mr-1.5 h-4 w-4" /> Run
            </UiButton>
          </div>
        </div>
      </div>
    </Teleport>

    <div v-if="loading" class="flex-1 flex items-center justify-center text-sm text-muted-foreground">Loading…</div>

    <template v-else-if="pipeline">
      <div class="flex gap-0 flex-1 min-h-0 bg-muted/50 rounded-2xl overflow-hidden">
        <!-- ====== Left Sidebar ====== -->
        <div class="scrollbar-sidebar w-[18rem] shrink-0 flex flex-col overflow-y-auto p-4" @click="handleSidebarClick">
          <div class="space-y-0.5 mt-1">
            <button
              class="w-full text-left px-3 py-2 rounded-lg text-sm transition-colors"
              :class="activeTab === 'stage' ? 'bg-card shadow-xs font-medium text-foreground' : 'text-muted-foreground hover:text-foreground hover:bg-card/50'"
              @click="navigateTab('stage')"
            >
              Stage View
            </button>
            <button
              class="w-full text-left px-3 py-2 rounded-lg text-sm transition-colors flex items-center justify-between"
              :class="activeTab === 'history' ? 'bg-card shadow-xs font-medium text-foreground' : 'text-muted-foreground hover:text-foreground hover:bg-card/50'"
              @click="navigateTab('history')"
            >
              <span>Build History</span>
              <span v-if="jobs.length > 0" class="text-xs tabular-nums">{{ jobs.length }}</span>
            </button>
            <button
              class="w-full text-left px-3 py-2 rounded-lg text-sm transition-colors"
              :class="activeTab === 'definition' ? 'bg-card shadow-xs font-medium text-foreground' : 'text-muted-foreground hover:text-foreground hover:bg-card/50'"
              @click="navigateTab('definition')"
            >
              Definition
            </button>
            <button
              class="w-full text-left px-3 py-2 rounded-lg text-sm transition-colors flex items-center justify-between"
              :class="activeTab === 'trigger-events' ? 'bg-card shadow-xs font-medium text-foreground' : 'text-muted-foreground hover:text-foreground hover:bg-card/50'"
              @click="navigateTab('trigger-events')"
            >
              <span>Trigger events</span>
              <span v-if="triggerEventsTotal > 0" class="text-xs tabular-nums">{{ triggerEventsTotal }}</span>
            </button>
          </div>
          <div class="mt-10 pt-5 border-t border-border/40 px-3">
            <!-- Section heading + summary strip -->
            <div class="flex items-center justify-between mb-3">
              <div class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Recent Builds</div>
              <div v-if="jobs.length > 0" class="flex items-center gap-2 text-xs tabular-nums">
                <span class="flex items-center gap-1 text-emerald-500 font-medium"><span class="w-1.5 h-1.5 rounded-full bg-emerald-500" />{{ successCount }}</span>
                <span class="flex items-center gap-1 text-red-500 font-medium"><span class="w-1.5 h-1.5 rounded-full bg-red-500" />{{ failedCount }}</span>
                <span v-if="runningCount > 0" class="flex items-center gap-1 text-amber-500 font-medium"><span class="w-1.5 h-1.5 rounded-full bg-amber-400 animate-pulse" />{{ runningCount }}</span>
              </div>
            </div>

            <!-- Empty state -->
            <div v-if="jobs.length === 0" class="text-xs text-muted-foreground/50 py-2">No builds yet. Trigger a build to get started.</div>

            <!-- Grouped timeline -->
            <template v-else>
              <div v-for="group in timelineBuilds" :key="group.label" class="mb-4 last:mb-0">
                <!-- Date header -->
                <div class="text-xs font-medium text-muted-foreground/70 uppercase tracking-wider mb-1.5 pl-0.5">{{ group.label }}</div>

                <!-- Entries -->
                <div class="space-y-0.5">
                  <div
                    v-for="j in group.jobs"
                    :key="j.id"
                    class="rounded-lg transition-colors hover:bg-muted/60 group flex items-start gap-2 py-2 pl-2.5 pr-2"
                    :class="j.status === 'running' ? 'ring-1 ring-amber-400/30 bg-amber-500/4' : ''"
                    @click="navToJob(j.id)"
                  >
                    <!-- Status icon -->
                    <span class="shrink-0 mt-px">
                      <CheckCircle2 v-if="j.status === 'success'" class="h-3.5 w-3.5 text-emerald-500" />
                      <XCircle v-else-if="j.status === 'failed'" class="h-3.5 w-3.5 text-red-500" />
                      <XCircle v-else-if="j.status === 'cancelled'" class="h-3.5 w-3.5 text-slate-400" />
                      <Loader2 v-else-if="j.status === 'running'" class="h-3.5 w-3.5 text-amber-500 animate-spin" />
                      <Clock v-else class="h-3.5 w-3.5" :class="j.status === 'queued' ? 'text-purple-400' : 'text-slate-300'" />
                    </span>

                    <div class="flex-1 min-w-0"><!-- Row: #N + status + ... menu -->
                      <div class="flex items-center gap-1.5">
                        <span class="text-sm font-mono font-semibold tabular-nums text-foreground">#{{ runNum(j, []) }}</span>

                        <span
                          v-if="j.status === 'running'"
                          class="text-xs font-mono tabular-nums text-amber-500/90"
                        >Running {{ runningDuration(j) }}</span>
                        <span
                          v-else-if="j.status === 'pending'"
                          class="text-xs text-slate-400/80"
                        >Pending</span>
                        <span
                          v-else-if="j.status === 'queued'"
                          class="text-xs text-purple-400/80"
                        >Queued</span>
                        <span
                          v-else-if="j.status === 'cancelled'"
                          class="text-xs text-slate-400/80"
                        >Cancelled</span>
                        <span
                          v-else
                          class="text-xs font-mono tabular-nums"
                          :class="j.status === 'success' ? 'text-emerald-600/80' : 'text-red-500/80'"
                        >{{ j.status === 'success' ? 'Passed' : 'Failed' }}</span>

                        <!-- Compact Verbose / Debug — square letter -->
                        <span
                          v-if="j.verbose_logs"
                          class="inline-flex h-3.5 w-3.5 items-center justify-center rounded-xs border border-sky-500/40 text-[9px] font-mono leading-none lowercase text-sky-600"
                          title="Verbose mode: extra per-step detail in logs; secrets stay redacted."
                        >v</span>
                        <span
                          v-if="j.debug_unredact"
                          class="inline-flex h-3.5 w-3.5 items-center justify-center rounded-xs border border-amber-500/45 text-[9px] font-mono leading-none lowercase text-amber-700"
                          title="Debug mode: logs are unredacted, platform-admin-only, and purged after 24h."
                        >d</span>

                        <!-- ... menu trigger (always visible) -->
                        <div class="ml-auto sidebar-job-menu relative">
                          <button
                            class="flex items-center justify-center w-6 h-6 rounded-md hover:bg-muted transition-all"
                            :class="showMenuForJob === j.id ? 'bg-muted' : ''"
                            @click="toggleJobMenu(j.id, $event)"
                            title="Actions"
                          >
                            <MoreHorizontal class="h-3.5 w-3.5 text-muted-foreground" />
                          </button>

                          <!-- Dropdown -->
                          <div
                            v-if="showMenuForJob === j.id"
                            class="absolute right-0 top-full mt-1 w-36 z-30 rounded-lg border border-border bg-card shadow-xl py-1 text-xs"
                          >
                            <button
                              v-if="j.status === 'pending' || j.status === 'running' || j.status === 'queued'"
                              class="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:bg-muted transition-colors text-red-500"
                              @click="cancelSidebarJob(j.id)"
                            >
                              <X class="h-3.5 w-3.5" />
                              Cancel
                            </button>
                            <button
                              class="w-full flex items-center gap-2 px-3 py-1.5 text-left hover:bg-muted transition-colors"
                              @click="viewJobLogs(j.id)"
                            >
                              <Eye class="h-3.5 w-3.5" />
                              View logs
                            </button>
                          </div>
                        </div>
                      </div>

                      <!-- Runner labels — own row; show at most 2, rest as ellipsis -->
                      <div
                        v-if="j.snapshot?.runner_labels?.length"
                        class="flex items-center gap-1 mt-1 min-w-0"
                      >
                        <span
                          v-for="l in j.snapshot.runner_labels.slice(0, 2)"
                          :key="l"
                          class="mono-chip text-[10px] text-muted-foreground/60 max-w-28 truncate"
                          :title="l"
                        >{{ l }}</span>
                        <span
                          v-if="j.snapshot.runner_labels.length > 2"
                          class="text-[10px] text-muted-foreground/45 shrink-0 select-none"
                          :title="j.snapshot.runner_labels.slice(2).join(', ')"
                        >…</span>
                      </div>

                      <!-- Build time: timestamp · duration -->
                      <div class="flex items-center gap-1.5 mt-0.5">
                        <span class="text-xs text-muted-foreground/35">{{ fmtTime(j.created_at) }}</span>
                        <span class="text-xs text-muted-foreground/20 select-none">·</span>
                        <span v-if="j.start_time && j.end_time" class="text-xs font-mono tabular-nums text-muted-foreground/65 font-medium">
                          {{ fmtDuration(j.start_time, j.end_time) }}
                        </span>
                        <span v-else-if="j.start_time && j.status === 'running'" class="text-xs font-mono tabular-nums text-amber-500/70">
                          {{ runningDuration(j) }}
                        </span>
                      </div>
                    </div>
                  </div>
                </div>
              </div>
            </template>
          </div>
        </div>

        <!-- ====== Right Content ====== -->
        <div class="flex-1 min-w-0 overflow-y-auto p-6">

          <!-- ======== STAGE VIEW ======== -->
          <div v-if="activeTab === 'stage'">
            <div v-if="!(pipeline.tasks || []).length" class="flex flex-col items-center justify-center py-24 text-muted-foreground">
              <Clock class="h-10 w-10 mb-3 text-muted-foreground/20" />
              <p class="text-sm">No stages defined</p>
            </div>
            <div v-else>
              <!-- Overview card -->
              <div class="rounded-xl border border-border/50 bg-card p-5 mb-5 flex items-center gap-6 shadow-xs">
                <div class="flex items-center gap-5">
                  <div class="text-center min-w-12">
                    <div class="text-lg font-bold tabular-nums text-emerald-500">{{ successCount }}</div>
                    <div class="text-xs text-muted-foreground mt-0.5">Passed</div>
                  </div>
                  <div class="text-center min-w-12">
                    <div class="text-lg font-bold tabular-nums text-red-500">{{ failedCount }}</div>
                    <div class="text-xs text-muted-foreground mt-0.5">Failed</div>
                  </div>
                  <div class="text-center min-w-12">
                    <div class="text-lg font-bold tabular-nums text-foreground">{{ jobs.length }}</div>
                    <div class="text-xs text-muted-foreground mt-0.5">Total</div>
                  </div>
                </div>
                <div class="w-px h-10 bg-border/30" />
                <div v-if="latestRun" class="min-w-0">
                  <div class="text-xs text-muted-foreground mb-1.5">Last build</div>
                  <div class="flex items-center gap-2 text-sm">
                    <span class="font-mono font-semibold text-primary tabular-nums">#{{ runNum(latestRun, []) }}</span>
                    <span
                      :class="cn(
                        'inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium',
                        latestRun.status === 'success' ? 'bg-emerald-500/10 text-emerald-600' :
                        latestRun.status === 'failed' ? 'bg-red-500/10 text-red-500' :
                        latestRun.status === 'running' ? 'bg-amber-500/10 text-amber-600' :
                        'bg-slate-500/10 text-slate-500'
                      )"
                    >
                      <span :class="cn('w-1.5 h-1.5 rounded-full', latestRun.status === 'success' ? 'bg-emerald-500' : latestRun.status === 'failed' ? 'bg-red-500' : latestRun.status === 'running' ? 'bg-amber-400 animate-pulse' : 'bg-slate-400')" />
                      {{ latestRun.status }}
                    </span>
                    <span class="font-mono text-xs text-muted-foreground">{{ fmtDuration(latestRun.start_time, latestRun.end_time) }}</span>
                    <span class="text-muted-foreground/40">·</span>
                    <span class="text-xs font-mono text-muted-foreground">{{ fmtTime(latestRun.start_time || latestRun.created_at) }}</span>
                  </div>
                </div>
                <div v-else class="text-xs text-muted-foreground">No builds yet</div>
              </div>

              <!-- Workflow graph — VueFlow + dagre (fixed-size steps; click to focus) -->
              <PipelineTopologyGraph
                v-model:selected-step="stageFocusStep"
                :tasks="(pipeline.tasks || []) as any[]"
                :step-statuses="latestRun?.step_statuses || null"
                :fmt-duration="(idx: number, ss: any[] | null) => fmtStepDuration(idx, ss)"
              />

              <!-- Focused step detail + logs -->
              <div
                v-if="stageFocusStep != null && stageFocusTask"
                class="mt-4 rounded-xl border border-border/50 bg-card shadow-xs overflow-hidden"
              >
                <div class="flex items-center gap-3 px-4 py-3 border-b border-border/30">
                  <span class="text-[10px] font-mono tabular-nums text-muted-foreground bg-muted rounded-md px-1.5 py-0.5">
                    {{ String(stageFocusStep + 1).padStart(2, '0') }}
                  </span>
                  <div class="min-w-0 flex-1">
                    <p class="text-sm font-semibold truncate" :title="stageFocusTask.name">{{ stageFocusTask.name }}</p>
                    <p class="text-[11px] text-muted-foreground uppercase tracking-wider">
                      {{ stageFocusTask.exec_type || '—' }}
                      <span class="mx-1 text-muted-foreground/40">·</span>
                      <span class="normal-case tracking-normal">{{ stageFocusStatus }}</span>
                      <span class="mx-1 text-muted-foreground/40">·</span>
                      <span class="font-mono normal-case tracking-normal">{{ fmtStepDuration(stageFocusStep, latestRun?.step_statuses || null) }}</span>
                    </p>
                  </div>
                  <RouterLink
                    v-if="latestRun"
                    :to="'/jobs/' + latestRun.id + '?step=' + stageFocusStep"
                    class="text-[11px] text-primary hover:underline shrink-0"
                  >Open job</RouterLink>
                  <button
                    type="button"
                    class="inline-flex h-7 w-7 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground"
                    title="Clear focus"
                    @click="stageFocusStep = null"
                  >
                    <X class="h-3.5 w-3.5" />
                  </button>
                </div>
                <div v-if="!latestRun" class="px-4 py-8 text-center text-sm text-muted-foreground">
                  No builds yet — run the pipeline to see step logs here.
                </div>
                <div
                  v-else-if="latestRun.debug_unredact && !auth.isPlatformAdmin"
                  class="px-4 py-6 text-center text-sm text-amber-800 dark:text-amber-200"
                >
                  This run used Debug mode — logs are visible to platform admins only.
                </div>
                <div v-else-if="stageFocusStatus === 'pending'" class="px-4 py-8 text-center text-sm text-muted-foreground">
                  Step has not started yet.
                </div>
                <div v-else class="p-3">
                  <div class="rounded-lg border border-border/60 overflow-hidden" style="height: 280px">
                    <StepLogPanel
                      :key="'stage-' + latestRun.id + '-' + stageFocusStep"
                      :logs="getHistoryLogs(latestRun.id)"
                      :step-order="stageFocusStep"
                    />
                  </div>
                </div>
              </div>
            </div>
          </div>

          <!-- ======== BUILD HISTORY ======== -->
          <div v-if="activeTab === 'history'">
            <div v-if="!(pipeline.tasks || []).length" class="flex flex-col items-center justify-center py-24 text-muted-foreground">
              <Clock class="h-10 w-10 mb-3 text-muted-foreground/20" />
              <p class="text-sm">No stages defined</p>
            </div>
            <template v-else>
              <div class="flex items-center gap-4 mb-4">
                <div class="text-base font-semibold text-foreground">Build History</div>
                <div class="flex items-center gap-3 ml-auto text-xs">
                  <span class="flex items-center gap-1 text-emerald-600"><CheckCircle2 class="h-3 w-3" /><span class="font-semibold tabular-nums">{{ successCount }}</span></span>
                  <span class="flex items-center gap-1 text-red-500"><XCircle class="h-3 w-3" /><span class="font-semibold tabular-nums">{{ failedCount }}</span></span>
                  <span class="text-muted-foreground">{{ jobs.length }} total</span>
                </div>
              </div>

              <div v-if="jobs.length === 0" class="rounded-xl border border-border/50 bg-card shadow-xs flex flex-col items-center justify-center py-20 text-muted-foreground">
                <Clock class="h-10 w-10 mb-3 text-muted-foreground/20" />
                <p class="text-sm">No builds yet</p>
              </div>

              <!-- Vertical run cards -->
              <div v-else class="space-y-3">
                <BuildHistoryCard
                  v-for="j in historyPagedJobs"
                  :key="j.id"
                  :job="j"
                  :run-number="runNum(j, historyPagedJobs)"
                  :logs="getHistoryLogs(j.id)"
                  :can-view-debug-logs="auth.isPlatformAdmin"
                  :initially-expanded="historyFocusJobId === j.id"
                  :highlighted="historyFocusJobId === j.id"
                  @expand="loadHistoryLogs($event)"
                />

                <!-- Pagination -->
                <div v-if="historyTotalPages > 1" class="flex items-center justify-between px-4 py-2.5 rounded-xl border border-border/50 bg-card">
                  <span class="text-xs text-muted-foreground">
                    {{ (historyPage - 1) * historyPageSize + 1 }}–{{ Math.min(historyPage * historyPageSize, jobs.length) }} of {{ jobs.length }}
                  </span>
                  <div class="flex items-center gap-1">
                    <button
                      class="px-2.5 py-1 rounded-sm text-xs transition-colors"
                      :class="historyPage === 1 ? 'text-muted-foreground/40 cursor-default' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
                      :disabled="historyPage === 1"
                      @click="historyGo(historyPage - 1)"
                    >Prev</button>
                    <button
                      v-for="p in Math.min(historyTotalPages, 7)"
                      :key="'pg-'+p"
                      class="w-7 h-7 rounded-sm text-xs tabular-nums transition-colors"
                      :class="p === historyPage ? 'bg-primary text-primary-foreground font-semibold' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
                      @click="historyGo(p)"
                    >{{ p }}</button>
                    <button
                      class="px-2.5 py-1 rounded-sm text-xs transition-colors"
                      :class="historyPage === historyTotalPages ? 'text-muted-foreground/40 cursor-default' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
                      :disabled="historyPage === historyTotalPages"
                      @click="historyGo(historyPage + 1)"
                    >Next</button>
                  </div>
                </div>
              </div>
            </template>
          </div>

          <!-- ======== DEFINITION ======== -->
          <div v-if="activeTab === 'definition'">
            <div
              v-if="yamlSyncBanner"
              class="mb-3 flex items-start gap-2.5 rounded-xl border px-3.5 py-3 text-sm leading-relaxed"
              :class="
                yamlSyncBanner.tone === 'destructive'
                  ? 'border-rose-500/50 bg-rose-100 text-rose-950 dark:border-rose-400/40 dark:bg-rose-950 dark:text-rose-50'
                  : 'border-amber-500/60 bg-amber-100 text-amber-950 dark:border-amber-400/40 dark:bg-amber-950 dark:text-amber-50'
              "
            >
              <AlertCircle
                v-if="yamlSyncBanner.tone === 'destructive'"
                class="mt-0.5 h-4 w-4 shrink-0 text-rose-800 dark:text-rose-300"
              />
              <AlertTriangle
                v-else
                class="mt-0.5 h-4 w-4 shrink-0 text-amber-800 dark:text-amber-300"
              />
              <div class="min-w-0 flex-1 space-y-1">
                <div class="flex flex-wrap items-baseline gap-x-2.5 gap-y-0.5">
                  <span class="font-semibold">{{ yamlSyncBanner.title }}</span>
                  <span
                    v-if="defYamlFilename || yamlSync?.filename"
                    class="font-mono text-xs opacity-75"
                  >{{ defYamlFilename || yamlSync?.filename }}</span>
                </div>
                <p v-if="yamlSyncBanner.detail" class="opacity-90">{{ yamlSyncBanner.detail }}</p>
                <div
                  v-if="yamlSyncBanner.tone === 'warning' && yamlSync?.status === 'stale'"
                  class="flex flex-wrap gap-x-3 gap-y-0.5 text-xs opacity-75"
                >
                  <span v-if="yamlSync.last_synced_at">Last synced {{ yamlSync.last_synced_at }}</span>
                  <span v-if="yamlSync.file_mtime">File {{ yamlSync.file_mtime }}</span>
                </div>
                <ul
                  v-if="yamlSyncBanner.errors?.length"
                  class="space-y-0.5 list-disc pl-4 font-mono text-xs opacity-95"
                >
                  <li v-for="(err, i) in yamlSyncBanner.errors.slice(0, 8)" :key="i">{{ err }}</li>
                </ul>
              </div>
            </div>

            <div
              v-if="defYamlLoading || (pipeline?.source === 'yaml' && defYamlAuthority === null)"
              class="rounded-xl border border-border/40 bg-card px-5 py-16 text-center text-sm text-muted-foreground"
            >
              Loading definition from YAML file…
            </div>

            <div
              v-else-if="pipeline?.source === 'yaml' && defYamlAuthority === 'unavailable'"
              class="rounded-xl border border-destructive/30 bg-card px-5 py-10 text-center space-y-3"
            >
              <p class="text-sm text-destructive font-medium">Definition unavailable</p>
              <p class="text-xs text-muted-foreground max-w-md mx-auto leading-relaxed">
                Fix the on-disk YAML (missing or invalid), then reopen this tab. DB projection is not shown as authority.
              </p>
              <button
                class="inline-flex items-center gap-1 text-xs font-medium text-primary hover:text-primary/80 transition-colors"
                @click="openEditDialog"
              >
                <Pencil class="h-3 w-3" />
                Edit YAML
              </button>
            </div>

            <div
              v-else-if="definitionView"
              class="rounded-xl border border-border/40 bg-card overflow-hidden shadow-[0_1px_3px_rgba(0,0,0,0.04)]"
            >
              <!-- ── Header ── -->
              <div class="px-5 pt-4 pb-3 flex items-start justify-between gap-4">
                <div class="min-w-0 flex-1">
                  <div class="flex items-center gap-2.5 flex-wrap">
                    <h2 class="text-sm font-semibold tracking-tight">{{ definitionView.name }}</h2>
                    <span class="mono-chip text-xs text-muted-foreground opacity-60 select-all">{{ definitionView.id?.slice(-8) }}</span>
                  </div>
                  <p v-if="definitionView.description" class="text-xs text-muted-foreground mt-1.5 leading-relaxed">{{ definitionView.description }}</p>
                </div>
                <div class="flex items-center gap-2 shrink-0 pt-0.5">
                  <span
                    v-if="definitionView.authority === 'file'"
                    class="text-xs text-muted-foreground bg-muted/60 rounded-sm px-2 py-0.5 font-medium"
                  >from file</span>
                  <span
                    v-else-if="hasYamlSource"
                    class="text-xs text-muted-foreground bg-muted/60 rounded-sm px-2 py-0.5 font-medium"
                  >from YAML</span>
                  <button
                    class="inline-flex items-center gap-1 text-xs font-medium text-primary hover:text-primary/80 transition-colors"
                    @click="openEditDialog"
                  >
                    <Pencil class="h-3 w-3" />
                    Edit YAML
                  </button>
                </div>
              </div>

              <!-- ── Four-card grid ── -->
              <div class="grid grid-cols-1 sm:grid-cols-2 xl:grid-cols-4">
                <!-- Trigger -->
                <div class="p-4">
                  <div class="flex items-center gap-1.5 mb-3">
                    <Zap class="h-3.5 w-3.5 text-amber-500" />
                    <span class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Trigger</span>
                  </div>
                  <div class="space-y-1.5">
                    <span class="mono-chip text-xs font-semibold px-2">{{ definitionView.trigger_type?.toUpperCase() }}</span>
                    <div v-if="definitionView.webhook_path" class="flex items-center gap-1.5">
                      <span class="text-xs text-muted-foreground/60 font-mono w-10 shrink-0">path</span>
                      <code class="text-xs font-mono text-foreground/70">{{ definitionView.webhook_path }}</code>
                      <span v-if="definitionView.webhook_environment" class="text-xs text-muted-foreground">default: {{ definitionView.webhook_environment }}</span>
                    </div>
                    <div v-if="definitionView.cron_expr" class="flex items-center gap-1.5">
                      <span class="text-xs text-muted-foreground/60 font-mono w-10 shrink-0">cron</span>
                      <code class="text-xs font-mono text-foreground/70">{{ definitionView.cron_expr }}</code>
                      <span v-if="definitionView.cron_environment" class="text-xs text-muted-foreground">scheduled target: {{ definitionView.cron_environment }}</span>
                    </div>
                    <div v-if="!definitionView.webhook_path && !definitionView.cron_expr" class="text-xs text-muted-foreground/50 italic">No external trigger configured</div>
                  </div>
                </div>

                <!-- Runner -->
                <div class="p-4 border-l border-border/15">
                  <div class="flex items-center gap-1.5 mb-3">
                    <Server class="h-3.5 w-3.5 text-blue-500" />
                    <span class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Runner</span>
                    <span v-if="(definitionView.runner_labels || []).length" class="text-xs text-muted-foreground/50 ml-auto">{{ definitionView.runner_labels.length }} label{{ definitionView.runner_labels.length !== 1 ? 's' : '' }}</span>
                  </div>
                  <div class="space-y-2">
                    <div class="flex flex-wrap gap-1">
                      <span v-for="l in (definitionView.runner_labels || [])" :key="l" class="mono-chip text-xs">{{ l }}</span>
                      <span v-if="!(definitionView.runner_labels || []).length" class="text-xs text-muted-foreground/50 italic">No labels set</span>
                    </div>
                    <div class="flex items-center gap-1.5">
                      <Clock class="h-3 w-3 text-muted-foreground/50" />
                      <span class="font-mono text-xs text-muted-foreground font-medium">{{ definitionView.timeout_seconds }}s</span>
                      <span class="text-xs text-muted-foreground/50">timeout</span>
                    </div>
                  </div>
                </div>

                <!-- Bindings -->
                <div class="p-4 border-l border-border/15">
                  <div class="flex items-center gap-1.5 mb-3">
                    <Link class="h-3.5 w-3.5 text-violet-500" />
                    <span class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Bindings</span>
                  </div>
                  <div class="space-y-2.5">
                    <div>
                      <div class="flex items-center gap-1.5 mb-1">
                        <span class="h-1.5 w-1.5 rounded-full bg-emerald-500/60"></span>
                        <span class="text-xs text-muted-foreground/60 uppercase tracking-wider">Variables</span>
                        <span v-if="definitionView.variableNames.length" class="text-xs text-muted-foreground/40 ml-auto">{{ definitionView.variableNames.length }}</span>
                      </div>
                      <div v-if="definitionView.variableNames.length === 0" class="text-xs text-muted-foreground/40 pl-3.5">No variables bound</div>
                      <div v-else class="flex flex-wrap gap-1 pl-3.5">
                        <code v-for="v in definitionView.variableNames" :key="v" class="mono-chip text-xs">{{ v }}</code>
                      </div>
                    </div>
                    <div>
                      <div class="flex items-center gap-1.5 mb-1">
                        <span class="h-1.5 w-1.5 rounded-full bg-rose-500/60"></span>
                        <span class="text-xs text-muted-foreground/60 uppercase tracking-wider">Secrets</span>
                        <span v-if="definitionView.secretNames.length" class="text-xs text-muted-foreground/40 ml-auto">{{ definitionView.secretNames.length }}</span>
                      </div>
                      <div v-if="definitionView.secretNames.length === 0" class="text-xs text-muted-foreground/40 pl-3.5">No secrets bound</div>
                      <div v-else class="flex flex-wrap gap-1 pl-3.5">
                        <code v-for="s in definitionView.secretNames" :key="s" class="mono-chip text-xs">{{ s }}</code>
                      </div>
                    </div>
                  </div>
                </div>

                <!-- Config -->
                <div class="p-4 border-l border-border/15">
                  <div class="flex items-center gap-1.5 mb-3">
                    <Settings class="h-3.5 w-3.5 text-muted-foreground/70" />
                    <span class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Config</span>
                  </div>
                  <div class="space-y-2">
                    <div v-if="definitionView.team_id" class="flex items-center gap-1.5">
                      <span class="text-xs text-muted-foreground/60 font-mono w-10 shrink-0">team</span>
                      <code class="mono-chip text-xs truncate">{{ teamLabel(definitionView.team_id) }}</code>
                    </div>
                    <div class="flex items-center gap-1.5">
                      <span class="text-xs text-muted-foreground/60 font-mono w-10 shrink-0">source</span>
                      <span class="mono-chip text-xs">{{ definitionView.source || 'form' }}</span>
                    </div>
                    <div v-if="definitionView.filename" class="flex items-center gap-1.5">
                      <span class="text-xs text-muted-foreground/60 font-mono w-10 shrink-0">file</span>
                      <code class="mono-chip text-xs truncate">{{ definitionView.filename }}</code>
                    </div>
                  </div>
                </div>
              </div>

              <!-- ── Concurrency (always shown) ── -->
              <div v-if="pipelineConcurrency" class="border-t border-border/20 px-5 py-4">
                <div class="flex items-center gap-2 mb-3">
                  <List class="h-3.5 w-3.5 text-purple-500" />
                  <span class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Concurrency</span>
                  <button
                    type="button"
                    class="inline-flex items-center text-muted-foreground/50 hover:text-foreground transition-colors"
                    :aria-expanded="showConcurrencyHelp"
                    aria-label="Concurrency help"
                    @click="showConcurrencyHelp = !showConcurrencyHelp"
                  >
                    <CircleHelp class="h-3.5 w-3.5" />
                  </button>
                  <button
                    type="button"
                    class="ml-auto inline-flex items-center gap-1 text-xs font-medium text-primary hover:text-primary/80 transition-colors"
                    @click="openEditDialog"
                  >
                    <Pencil class="h-3 w-3" />
                    Edit YAML
                  </button>
                </div>

                <template v-if="pipelineConcurrency.configured">
                  <div class="flex flex-wrap items-center gap-2 text-xs">
                    <span class="text-muted-foreground">Group</span>
                    <code class="mono-chip text-xs">{{ pipelineConcurrency.group }}</code>
                    <span class="text-muted-foreground ml-1">Mode</span>
                    <span
                      class="font-semibold rounded-sm px-1.5 py-0.5 border"
                      :class="{
                        'bg-amber-500/10 text-amber-600 border-amber-500/20': pipelineConcurrency.mode === 'queue',
                        'bg-blue-500/10 text-blue-600 border-blue-500/20': pipelineConcurrency.mode === 'discard-new',
                        'bg-emerald-500/10 text-emerald-600 border-emerald-500/20': pipelineConcurrency.mode === 'discard-old',
                      }"
                    >{{ pipelineConcurrency.mode }}</span>
                    <template v-if="pipelineConcurrency.mode === 'queue'">
                      <span class="text-muted-foreground ml-1">Max queue</span>
                      <span class="font-mono tabular-nums text-foreground/80">{{ pipelineConcurrency.maxQueue ?? '—' }}</span>
                      <span class="text-muted-foreground ml-1">Queue timeout</span>
                      <span class="font-mono tabular-nums text-foreground/80">{{ formatConcurrencyTimeout(pipelineConcurrency.queueTimeoutSec) }}</span>
                    </template>
                  </div>
                </template>
                <template v-else>
                  <div class="flex flex-wrap items-center gap-2 text-xs">
                    <span class="text-muted-foreground">Status</span>
                    <span class="font-semibold rounded-sm px-1.5 py-0.5 border bg-muted text-muted-foreground border-border/50">not configured</span>
                    <span class="text-muted-foreground ml-1">Default mode if enabled</span>
                    <span class="font-mono rounded-sm px-1.5 py-0.5 border bg-emerald-500/10 text-emerald-700 border-emerald-500/20">discard-old</span>
                    <span class="text-muted-foreground ml-1">Max queue</span>
                    <span class="font-mono tabular-nums text-foreground/80">3</span>
                    <span class="text-muted-foreground ml-1">Queue timeout</span>
                    <span class="font-mono tabular-nums text-foreground/80">30m</span>
                  </div>
                </template>

                <p class="mt-2 text-xs text-muted-foreground leading-relaxed">
                  {{ pipelineConcurrencyHint(pipelineConcurrency.configured, pipelineConcurrency.mode) }}
                </p>
                <div
                  v-if="showConcurrencyHelp"
                  class="mt-3 rounded-lg border border-border/50 bg-muted/30 px-3 py-2.5 text-xs text-muted-foreground leading-relaxed space-y-2"
                >
                  <p class="font-medium text-foreground/80">Adjustable fields</p>
                  <ul class="list-disc pl-4 space-y-1">
                    <li><code class="font-mono text-[10px]">concurrency.group</code> — required to enable; shared lock name</li>
                    <li><code class="font-mono text-[10px]">mode</code> — <code class="font-mono text-[10px]">discard-old</code> (YAML default), <code class="font-mono text-[10px]">queue</code>, or <code class="font-mono text-[10px]">discard-new</code></li>
                    <li><code class="font-mono text-[10px]">max-queue</code> (1–100, default 3) — only when mode is <code class="font-mono text-[10px]">queue</code></li>
                    <li><code class="font-mono text-[10px]">queue-timeout</code> (default 30m) — only when mode is <code class="font-mono text-[10px]">queue</code></li>
                  </ul>
                  <p>
                    Change via
                    <button type="button" class="text-primary underline underline-offset-2" @click="openEditDialog">Edit YAML</button>
                    (<code class="font-mono text-[10px]">concurrency:</code> block). Save writes the file and applies immediately.

                    Applies to new jobs; already-created job snapshots stay frozen.
                  </p>
                </div>
              </div>

              <!-- ── Notify (always shown) ── -->
              <div class="border-t border-border/20 px-5 py-4">
                <div class="flex items-center gap-2 mb-3">
                  <Bell class="h-3.5 w-3.5 text-sky-500" />
                  <span class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Notify</span>
                  <span v-if="pipelineNotify.length" class="text-xs text-muted-foreground/50 ml-auto">{{ pipelineNotify.length }} channel{{ pipelineNotify.length !== 1 ? 's' : '' }}</span>
                  <button
                    v-else
                    type="button"
                    class="ml-auto inline-flex items-center gap-1 text-xs font-medium text-primary hover:text-primary/80 transition-colors"
                    @click="openEditDialog"
                  >
                    <Pencil class="h-3 w-3" />
                    Edit YAML
                  </button>
                </div>

                <template v-if="pipelineNotify.length">
                  <div class="space-y-2">
                    <div
                      v-for="(ch, i) in pipelineNotify"
                      :key="i"
                      class="flex flex-wrap items-center gap-2 text-xs"
                    >
                      <span class="text-muted-foreground">Type</span>
                      <code class="mono-chip text-xs">{{ ch.type }}</code>
                      <span class="text-muted-foreground ml-1">Webhook</span>
                      <code class="mono-chip text-xs truncate max-w-[20rem]" :title="ch.webhook">{{ ch.webhook }}</code>
                      <span class="text-muted-foreground ml-1">On</span>
                      <span class="font-mono text-foreground/80">{{ (ch.on && ch.on.length ? ch.on : ['success', 'failure']).join(', ') }}</span>
                    </div>
                  </div>
                  <p class="mt-2 text-xs text-muted-foreground leading-relaxed">
                    Manager sends an end-of-build card when the job reaches a matching terminal status.
                    Set <code class="font-mono text-[10px]">server.public_base_url</code> for a View Job link.
                  </p>
                </template>
                <template v-else>
                  <div class="flex flex-wrap items-center gap-2 text-xs">
                    <span class="text-muted-foreground">Status</span>
                    <span class="font-semibold rounded-sm px-1.5 py-0.5 border bg-muted text-muted-foreground border-border/50">not configured</span>
                    <span class="text-muted-foreground ml-1">Add</span>
                    <code class="font-mono text-[10px] rounded-sm px-1.5 py-0.5 border bg-sky-500/10 text-sky-700 border-sky-500/20">notify:</code>
                    <span class="text-muted-foreground">in YAML (e.g. Feishu webhook via Secrets)</span>
                  </div>
                </template>
              </div>

              <!-- ── Declared deployment environments ── -->
              <div class="border-t border-border/20 px-5 py-4">
                <div class="flex items-center gap-2 mb-3">
                  <Server class="h-3.5 w-3.5 text-teal-500" />
                  <span class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Declared environments</span>
                  <span class="text-xs text-muted-foreground/50 ml-auto">{{ definitionView.environments.length }}</span>
                </div>
                <div v-if="definitionView.environments.length" class="flex flex-wrap gap-2">
                  <code v-for="slug in definitionView.environments" :key="slug" class="mono-chip text-xs">{{ slug }}</code>
                </div>
                <p v-else class="text-xs text-destructive">No environments declared; this pipeline cannot be triggered.</p>
                <p class="mt-2 text-[11px] text-muted-foreground">Every run must select one of these registered environment slugs. Manual inputs named “environment” do not select the deployment environment.</p>
              </div>

              <!-- ── Environment (pipeline-level env:) ── -->
              <div
                v-if="definitionView.envEntries.length"
                class="border-t border-border/20 px-5 py-4"
              >
                <div class="flex items-center gap-2 mb-3">
                  <Braces class="h-3.5 w-3.5 text-teal-500" />
                  <span class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Environment</span>
                  <span class="text-xs text-muted-foreground/50 ml-auto">{{ definitionView.envEntries.length }}</span>
                </div>
                <dl class="space-y-2">
                  <div
                    v-for="e in definitionView.envEntries"
                    :key="e.name"
                    class="grid grid-cols-[minmax(8rem,12rem)_1fr] gap-x-4 gap-y-0.5 items-baseline text-xs"
                  >
                    <dt class="font-mono text-muted-foreground truncate" :title="e.name">{{ e.name }}</dt>
                    <dd class="font-mono text-foreground/85 break-all leading-relaxed">{{ e.value || '—' }}</dd>
                  </div>
                </dl>
              </div>

              <!-- ── Parameters ── -->
              <div v-if="defHasInputDefs" class="border-t border-border/20 px-5 py-4">
                <div class="flex items-center gap-2 mb-3">
                  <Sliders class="h-3.5 w-3.5 text-orange-500" />
                  <span class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Parameters</span>
                  <span class="text-xs text-muted-foreground/50 ml-auto">{{ definitionView.input_defs.length }}</span>
                </div>
                <div class="divide-y divide-border/20 -mx-5">
                  <div
                    v-for="inp in definitionView.input_defs"
                    :key="inp.name"
                    class="px-5 py-2.5 flex items-center gap-3 text-xs"
                  >
                    <span class="font-mono font-semibold text-foreground w-36 shrink-0">{{ inp.name }}</span>
                    <span v-if="inp.description" class="text-muted-foreground min-w-0">{{ inp.description }}</span>
                    <span v-else class="italic text-muted-foreground/50">—</span>
                    <span class="flex-1" />
                    <span
                      v-if="inp.type || inp.pattern || inp.options?.length"
                      class="inline-flex items-center overflow-hidden rounded-full border border-sky-500/20 shrink-0 max-w-[20rem]"
                    >
                      <span
                        class="px-2 py-0.5 font-medium bg-sky-500/10 text-sky-600"
                        :title="typeHint(inp.type || '')"
                      >{{ inp.type || 'any' }}</span>
                      <span
                        v-if="inp.pattern"
                        class="px-2 py-0.5 font-mono text-[10px] bg-cyan-500/10 text-cyan-600 border-l border-cyan-500/20"
                        :title="`Regex — validated at trigger: /${inp.pattern}/`"
                      >/{{ inp.pattern }}/</span>
                      <span
                        v-if="inp.options?.length"
                        class="px-2 py-0.5 font-mono text-[10px] bg-teal-500/10 text-teal-600 border-l border-teal-500/20"
                        title="Allowed values"
                      >{{ inp.options.join(', ') }}</span>
                    </span>
                    <span
                      class="font-medium rounded-full px-2 py-0.5 uppercase tracking-wide shrink-0"
                      :class="inp.required ? 'bg-amber-500/10 text-amber-600 border border-amber-500/20' : 'bg-slate-500/10 text-slate-500 border border-slate-500/20'"
                    >{{ inp.required ? 'required' : 'optional' }}</span>
                    <span
                      v-if="inp.masking"
                      class="font-medium rounded-full px-2 py-0.5 uppercase tracking-wide shrink-0 bg-violet-500/10 text-violet-600 border border-violet-500/20"
                      title="Redacted in logs, audit, and job snapshots"
                    >masking</span>
                    <span v-if="inp.default && !inp.masking" class="text-muted-foreground font-mono shrink-0">= {{ inp.default }}</span>
                    <span v-else-if="inp.default && inp.masking" class="text-muted-foreground font-mono shrink-0">= ***</span>
                    <span v-else class="text-muted-foreground/35 italic font-mono shrink-0 select-none">= ?</span>
                  </div>
                </div>
              </div>

              <!-- ── Steps ── -->
              <div class="border-t border-border/20 px-5 py-4">
                <div class="flex items-center gap-2 mb-3">
                  <List class="h-3.5 w-3.5 text-indigo-500" />
                  <span class="text-xs font-semibold text-muted-foreground uppercase tracking-wider">Steps</span>
                  <span v-if="(definitionView.tasks || []).length" class="text-xs text-muted-foreground/50 ml-auto">{{ definitionView.tasks.length }}</span>
                </div>
                <div v-if="!(definitionView.tasks || []).length" class="py-8 text-center text-xs text-muted-foreground/50">
                  No steps defined.
                </div>
                <div v-else class="divide-y divide-border/20 -mx-5">
                  <div
                    v-for="(step, idx) in definitionView.tasks"
                    :key="idx"
                    class="px-5 py-3.5"
                  >
                    <!-- Step header: number, name, engine badge, flags, metadata -->
                    <div class="flex items-center gap-2 mb-2 flex-wrap text-xs">
                      <span class="font-mono font-bold text-muted-foreground/70 tabular-nums w-6">
                        {{ String(Number(idx) + 1).padStart(2, '0') }}
                      </span>
                      <span class="font-semibold text-foreground">{{ step.name }}</span>
                      <span class="mono-chip text-xs font-medium">{{ step.exec_type }}</span>

                      <!-- Timeout -->
                      <span v-if="step.timeout_seconds > 0" class="inline-flex items-center gap-1 text-muted-foreground/70">
                        <Clock class="h-3 w-3" />
                        <span class="font-mono">{{ step.timeout_seconds }}s</span>
                      </span>

                      <!-- Retry -->
                      <span v-if="step.retry_count > 0" class="inline-flex items-center gap-1 text-muted-foreground/70">
                        <RotateCw class="h-3 w-3" />
                        <span class="font-mono">{{ step.retry_count }}</span>
                      </span>

                      <span class="flex-1" />

                      <!-- Flags: continue-on-error, privileged -->
                      <span v-if="step.fail_strategy === 'continue'" class="font-medium text-amber-600 bg-amber-500/8 border border-amber-500/20 rounded-sm px-1.5 py-0.5">
                        continue-on-error
                      </span>
                      <span v-if="(step as any).privileged" class="font-semibold text-red-500 bg-red-500/8 border border-red-500/20 rounded-sm px-1.5 py-0.5 uppercase tracking-wider">
                        privileged
                      </span>
                      <span
                        v-if="(step as any).capabilities?.length"
                        class="font-semibold text-red-600 bg-red-500/10 border border-red-500/25 rounded-sm px-1.5 py-0.5"
                        title="Plugin-requested capabilities are not grants; each Runner administrator must separately authorize them locally."
                      >requests: {{ (step as any).capabilities.join(', ') }}</span>
                      <span
                        v-if="(step as any).capabilities?.includes('docker-socket')"
                        class="font-medium text-amber-700"
                      >High privilege: Runner admin must set <code>docker_socket_allowed = true</code> in local Runner config.</span>
                    </div>

                    <!-- Docker engine extras: image + volumes + pull secret -->
                    <div v-if="step.image || step.volumes || step.registry_secret" class="flex flex-wrap items-center gap-x-4 gap-y-1 mb-2 text-xs">
                      <span v-if="step.image" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground/60 text-xs w-8">image</span>
                        <code class="mono-chip text-xs">{{ step.image }}</code>
                      </span>
                      <span v-if="step.registry_secret" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground/60 text-xs">registry secret</span>
                        <code class="mono-chip text-xs">{{ step.registry_secret }}</code>
                        <span class="text-amber-600 dark:text-amber-400" title="Legacy compatibility only; use buz/docker-login before registry operations.">Deprecated · use buz/docker-login</span>
                      </span>
                      <span v-if="step.volumes" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground/60 text-xs w-14">volumes</span>
                        <code class="mono-chip text-xs">{{ Array.isArray(step.volumes) ? (step.volumes as string[]).join(', ') : step.volumes }}</code>
                      </span>
                    </div>

                    <!-- Nested config: deliver / render -->
                    <div
                      v-if="(step as any).config_source || (step as any).config_key_path || (step as any).config_dest || formatConfigRenderSummary((step as any).config_render)"
                      class="flex flex-wrap items-center gap-x-4 gap-y-1 mb-2 text-xs"
                    >
                      <span v-if="(step as any).config_source" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground/60 text-xs">config.source</span>
                        <code class="mono-chip text-xs">{{ (step as any).config_source }}</code>
                        <span class="text-amber-600 dark:text-amber-400" title="Legacy compatibility only; fetch with buz/config-fetch, then use config.render.">Deprecated · use buz/config-fetch</span>
                      </span>
                      <span v-if="(step as any).config_key_path" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground/60 text-xs">key_path</span>
                        <code class="mono-chip text-xs">{{ (step as any).config_key_path }}</code>
                        <span class="text-amber-600 dark:text-amber-400" title="Legacy compatibility only; pass a complete bundle URL to buz/config-fetch.">Deprecated · use buz/config-fetch</span>
                      </span>
                      <span v-if="(step as any).config_dest" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground/60 text-xs">dest</span>
                        <code class="mono-chip text-xs">{{ (step as any).config_dest }}</code>
                      </span>
                      <span
                        v-if="formatConfigRenderSummary((step as any).config_render)"
                        class="inline-flex items-center gap-1"
                      >
                        <span class="text-muted-foreground/60 text-xs">render</span>
                        <code class="mono-chip text-xs">{{ formatConfigRenderSummary((step as any).config_render) }}</code>
                      </span>
                    </div>

                    <!-- Step-level env: -->
                    <div
                      v-if="step.step_env && typeof step.step_env === 'object' && Object.keys(step.step_env).length"
                      class="mb-2"
                    >
                      <dl class="space-y-1">
                        <div
                          v-for="(val, key) in step.step_env"
                          :key="String(key)"
                          class="grid grid-cols-[minmax(5rem,9rem)_1fr] gap-x-3 items-baseline text-xs"
                        >
                          <dt class="font-mono text-muted-foreground/70 truncate" :title="String(key)">{{ key }}</dt>
                          <dd class="font-mono text-foreground/80 break-all leading-relaxed">{{ val }}</dd>
                        </div>
                      </dl>
                    </div>

                    <!-- Other step metadata: SSH hosts, host labels, dir -->
                    <div
                      v-if="sshHostDisplay(step) || step.host_labels || step.work_dir || step.hosts_strategy"
                      class="flex flex-wrap items-center gap-x-4 gap-y-1 mb-2 text-xs"
                    >
                      <span v-if="step.work_dir" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground/60 text-xs w-8">dir</span>
                        <code class="mono-chip text-xs">{{ step.work_dir }}</code>
                      </span>
                      <span v-if="sshHostDisplay(step)" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground/60 text-xs w-14">host</span>
                        <code class="mono-chip text-xs">{{ sshHostDisplay(step) }}</code>
                      </span>
                      <span v-if="step.host_labels" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground/60 text-xs w-14">labels</span>
                        <code class="mono-chip text-xs">{{ step.host_labels }}</code>
                      </span>
                      <span v-if="step.hosts_strategy" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground/60 text-xs w-16">strategy</span>
                        <code class="mono-chip text-xs">{{ step.hosts_strategy }}</code>
                      </span>
                    </div>

                    <div v-if="step.marketplace" class="mt-2 rounded-lg border border-border/40 bg-muted/20 px-2.5 py-2 text-[10px] font-mono text-muted-foreground/70 space-y-1">
                      <div class="flex flex-wrap items-center gap-x-2 gap-y-1">
                        <span class="font-semibold text-foreground/70">{{ step.marketplace.uses_ref }}</span>
                        <span v-if="step.marketplace.resolved_version">v{{ step.marketplace.resolved_version }}</span>
                        <span v-if="step.marketplace.reviewed != null" :class="step.marketplace.reviewed ? 'text-emerald-600' : 'text-amber-600'">{{ step.marketplace.reviewed ? 'reviewed' : 'unreviewed' }}</span>
                      </div>
                      <div v-if="step.marketplace.content_sha256" class="break-all">sha256:{{ step.marketplace.content_sha256 }}</div>
                      <div v-if="step.marketplace.source" class="break-all">source: {{ step.marketplace.source }}</div>
                      <div v-if="step.marketplace.inputs?.length" class="space-y-0.5">
                        <div class="text-[9px] uppercase tracking-wide text-muted-foreground/60">Declared plugin inputs</div>
                        <div v-for="input in step.marketplace.inputs" :key="input.name || input['name']" class="break-all">{{ input['name'] }}<span v-if="input['required'] === true || input['required'] === 'true'"> (required)</span><span v-if="input['description']"> — {{ input['description'] }}</span></div>
                      </div>
                      <div v-if="step.capabilities?.length" class="font-semibold text-red-600" title="Runner admins authorize capabilities separately in local Runner configuration.">Requests (not grants): {{ step.capabilities.join(', ') }}</div>
                      <div v-if="step.capabilities?.includes('docker-socket')" class="text-amber-700">High privilege: Docker socket access gives root-equivalent control of the Runner's Docker host. A Runner administrator must enable <code>docker_socket_allowed = true</code> locally.</div>
                      <div v-if="step.privileged" class="font-semibold text-red-600">High privilege: privileged container requested.</div>
                    </div>
                    <div v-else-if="step.uses" class="mt-2 rounded-lg border border-border/40 bg-muted/20 px-2.5 py-2 text-[10px] font-mono text-muted-foreground/70 space-y-1">
                      <div class="font-semibold text-foreground/70">{{ step.uses }}</div>
                      <div v-if="Object.keys(step.plugin_inputs || {}).length" class="space-y-0.5">
                        <div class="text-[9px] uppercase tracking-wide text-muted-foreground/60">Requested inputs</div>
                        <div v-for="(value, key) in step.plugin_inputs" :key="String(key)" class="break-all"><span class="text-muted-foreground/50">{{ key }}:</span> {{ value }}</div>
                      </div>
                      <div v-if="step.capabilities?.length" class="font-semibold text-red-600" title="Runner admins authorize capabilities separately in local Runner configuration.">Requests (not grants): {{ step.capabilities.join(', ') }}</div>
                      <div v-if="step.capabilities?.includes('docker-socket')" class="text-amber-700">High privilege: Docker socket access gives root-equivalent control of the Runner's Docker host. A Runner administrator must enable <code>docker_socket_allowed = true</code> locally.</div>
                      <div v-if="step.privileged" class="font-semibold text-red-600">High privilege: privileged container requested.</div>
                    </div>
                    <!-- Step secrets badge -->
                    <div v-if="step.step_secrets" class="flex items-center gap-1.5 mb-2 text-xs text-muted-foreground/70">
                      <span>step secrets:</span>
                      <code class="mono-chip text-xs">{{ step.step_secrets }}</code>
                    </div>

                    <!-- Command block — keep text node flush against tags so
                         whitespace-pre-wrap does not indent the first line -->
                    <div class="relative font-mono text-xs leading-relaxed text-foreground/80 bg-muted/30 rounded-lg px-3.5 py-2.5 whitespace-pre-wrap break-all border border-border/25"><div class="absolute left-0 top-1 bottom-1 w-0.5 rounded-full bg-border/30" aria-hidden="true" />{{ step.command_template }}</div>
                  </div>
                </div>
              </div>
            </div>
          </div>

          <!-- ======== TRIGGER EVENTS ======== -->
          <div v-if="activeTab === 'trigger-events'" class="space-y-4">
            <div class="rounded-xl border border-border/40 bg-card p-5 space-y-4">
              <div class="flex items-start justify-between gap-3 flex-wrap">
                <div>
                  <h2 class="text-base font-semibold tracking-tight flex items-center gap-2">
                    <Webhook class="h-4 w-4 text-muted-foreground" />
                    Trigger hook (git)
                  </h2>
                  <p class="text-xs text-muted-foreground mt-1 max-w-xl leading-relaxed">
                    Multiple hooks per pipeline (e.g. GitHub + Gitee). After create, settings are immutable —
                    delete and add a new hook to change them. Secret is shown once at create time.
                  </p>
                </div>
              </div>

              <div
                v-for="hook in gitHooks"
                :key="hook.id"
                class="rounded-lg border border-border/50 bg-muted/15 p-3 space-y-2"
              >
                <div class="flex items-center justify-between gap-2 flex-wrap">
                  <div class="flex items-center gap-2 min-w-0 flex-wrap">
                    <span class="mono-chip text-xs font-semibold uppercase">{{ hook.provider }}</span>
                    <span v-if="hook.label" class="text-sm font-medium truncate">{{ hook.label }}</span>
                    <span v-if="hook.branch_filter" class="text-xs font-mono text-muted-foreground">{{ hook.branch_filter }}</span>
                    <span v-else class="text-xs text-muted-foreground/60">all branches</span>
                    <span class="text-xs text-muted-foreground">environment: {{ hook.environment }}</span>
                  </div>
                  <UiButton size="sm" variant="ghost" @click="removeGitHook(hook)">Delete</UiButton>
                </div>
                <div class="flex items-center gap-2 min-w-0">
                  <span class="text-xs text-muted-foreground shrink-0 w-14">URL</span>
                  <code class="text-xs font-mono truncate flex-1">{{ windowOrigin }}{{ hook.url_path }}</code>
                  <button type="button" class="text-muted-foreground hover:text-foreground" title="Copy URL" @click="copyGitHookField(hook, 'url')">
                    <Check v-if="gitHookCopied?.id === hook.id && gitHookCopied.kind === 'url'" class="h-3.5 w-3.5 text-emerald-500" />
                    <Copy v-else class="h-3.5 w-3.5" />
                  </button>
                </div>
                <div v-if="gitHookSecretsOnce[hook.id]" class="flex items-center gap-2 min-w-0">
                  <span class="text-xs text-muted-foreground shrink-0 w-14">Secret</span>
                  <code class="text-xs font-mono truncate flex-1">{{ gitHookSecretsOnce[hook.id] }}</code>
                  <button type="button" class="text-muted-foreground hover:text-foreground" title="Copy secret" @click="copyGitHookField(hook, 'secret')">
                    <Check v-if="gitHookCopied?.id === hook.id && gitHookCopied.kind === 'secret'" class="h-3.5 w-3.5 text-emerald-500" />
                    <Copy v-else class="h-3.5 w-3.5" />
                  </button>
                </div>
                <p v-else class="text-[11px] text-muted-foreground pl-14">
                  Secret was shown once at create. To get a new secret, delete this hook and create again.
                </p>
              </div>

              <div class="rounded-lg border border-dashed border-border/60 p-3 space-y-3">
                <div class="text-xs font-medium text-muted-foreground uppercase tracking-wider">Add webhook</div>
                <div class="grid grid-cols-1 sm:grid-cols-2 gap-2">
                  <div>
                    <label class="text-[10px] uppercase text-muted-foreground tracking-wider mb-0.5 block">Label</label>
                    <input v-model="newHookLabel" type="text" placeholder="e.g. github-main" class="w-full rounded-lg border border-border bg-input px-2.5 py-1.5 text-sm outline-hidden" />
                  </div>
                  <div>
                    <label class="text-[10px] uppercase text-muted-foreground tracking-wider mb-0.5 block">Provider</label>
                    <select v-model="newHookProvider" class="w-full rounded-lg border border-border bg-input px-2.5 py-1.5 text-sm outline-hidden">
                      <option value="gitee">Gitee</option>
                      <option value="github">GitHub</option>
                    </select>
                  </div>
                  <div>
                    <label class="text-[10px] uppercase text-muted-foreground tracking-wider mb-0.5 block">Branch filter</label>
                    <input v-model="newHookBranchFilter" type="text" placeholder="optional" class="w-full rounded-lg border border-border bg-input px-2.5 py-1.5 text-sm font-mono outline-hidden" />
                  </div>
                  <div>
                    <label class="text-[10px] uppercase text-muted-foreground tracking-wider mb-0.5 block">Environment</label>
            <select
              v-model="newHookEnvironment"
              class="w-full rounded-lg border border-border bg-input px-2.5 py-1.5 text-sm outline-hidden"
              :disabled="environmentLoadFailed || newHookAvailableEnvironments.length === 0"
            >
                      <option value="" disabled>Select a declared environment…</option>
                      <option v-for="environment in newHookAvailableEnvironments" :key="environment.id" :value="environment.slug">
                        {{ environment.name }} ({{ environment.slug }})
                      </option>
                    </select>
                    <p v-if="environmentLoadFailed" class="mt-1 text-[11px] text-destructive">
                      Could not load registered environments. Retry after restoring Manager access.
                    </p>
                    <p v-else-if="newHookAvailableEnvironments.length === 0" class="mt-1 text-[11px] text-destructive">
                      This pipeline declares no environments registered in the Manager.
                    </p>
                  </div>
                </div>
                <UiButton size="sm" :disabled="gitHookSaving || environmentLoadFailed || newHookAvailableEnvironments.length === 0 || !newHookEnvironment" @click="addGitHook">
                  {{ gitHookSaving ? 'Creating…' : 'Add Git webhook' }}
                </UiButton>
              </div>
            </div>

            <div class="rounded-xl border border-border/40 bg-card overflow-hidden">
              <div class="px-5 py-3 border-b border-border/30 flex items-center gap-3 flex-wrap">
                <h2 class="text-sm font-semibold">Trigger events</h2>
                <select v-model="triggerFilterSource" class="rounded-lg border border-border/60 bg-background px-2 py-1 text-xs">
                  <option value="">All sources</option>
                  <option value="git">git</option>
                  <option value="webhook">webhook</option>
                  <option value="manual">manual</option>
                  <option value="cron">cron</option>
                </select>
                <select v-model="triggerFilterDecision" class="rounded-lg border border-border/60 bg-background px-2 py-1 text-xs">
                  <option value="">All decisions</option>
                  <option value="triggered">triggered</option>
                  <option value="skipped">skipped</option>
                  <option value="rejected">rejected</option>
                  <option value="failed">failed</option>
                </select>
                <button
                  type="button"
                  class="ml-auto text-xs text-muted-foreground hover:text-foreground inline-flex items-center gap-1"
                  @click="loadTriggerEvents"
                >
                  <RotateCw class="h-3 w-3" /> Refresh
                </button>
              </div>

              <div v-if="triggerEventsLoading" class="px-5 py-10 text-sm text-muted-foreground text-center">Loading…</div>
              <div v-else-if="triggerEvents.length === 0" class="px-5 py-12 text-center text-sm text-muted-foreground">
                No trigger events yet. Manual builds, webhooks, cron, and Git pushes appear here.
              </div>
              <div v-else class="overflow-x-auto">
                <table class="w-full text-[13px]">
                  <thead>
                    <tr class="border-b border-border/30 bg-muted/10 text-[11px] uppercase text-muted-foreground">
                      <th class="text-left px-4 py-2.5 font-medium">Time</th>
                      <th class="text-left px-4 py-2.5 font-medium">Source</th>
                      <th class="text-left px-4 py-2.5 font-medium">Ref</th>
                      <th class="text-left px-4 py-2.5 font-medium">Commit</th>
                      <th class="text-left px-4 py-2.5 font-medium">Decision</th>
                      <th class="text-left px-4 py-2.5 font-medium">Job</th>
                    </tr>
                  </thead>
                  <tbody class="divide-y divide-border/20">
                    <tr v-for="ev in triggerEvents" :key="ev.id" class="hover:bg-muted/5">
                      <td class="px-4 py-2.5 text-xs text-muted-foreground whitespace-nowrap font-mono">
                        {{ formatLocalShort(ev.received_at) }}
                      </td>
                      <td class="px-4 py-2.5 text-xs whitespace-nowrap">
                        <span class="font-medium">{{ ev.source }}</span>
                        <span v-if="ev.provider" class="text-muted-foreground"> · {{ ev.provider }}</span>
                      </td>
                      <td class="px-4 py-2.5 text-xs font-mono">
                        <span class="inline-flex items-center gap-1.5 flex-wrap">
                          <span v-if="ev.branch || ev.tag">{{ ev.branch || ev.tag }}</span>
                          <span v-else class="text-muted-foreground">—</span>
                          <span v-if="ev.commit_sha" class="mono-chip text-xs text-muted-foreground">{{ shortSha(ev.commit_sha) }}</span>
                        </span>
                      </td>
                      <td class="px-4 py-2.5 text-xs max-w-[220px]">
                        <div class="truncate" :title="ev.commit_message">{{ ev.commit_message || '—' }}</div>
                        <div v-if="ev.sender" class="text-[10px] text-muted-foreground">{{ ev.sender }}</div>
                      </td>
                      <td class="px-4 py-2.5 text-xs whitespace-nowrap">
                        <span :class="decisionClass(ev.decision)" class="font-medium">{{ ev.decision }}</span>
                        <span v-if="ev.skip_reason" class="text-muted-foreground ml-1">{{ ev.skip_reason }}</span>
                      </td>
                      <td class="px-4 py-2.5 text-xs whitespace-nowrap">
                        <RouterLink
                          v-if="ev.job_id"
                          :to="{
                            name: 'pipeline-detail',
                            params: { id: pipelineId, tab: 'history' },
                            query: { job: ev.job_id },
                          }"
                          class="text-primary hover:underline font-mono inline-flex items-center gap-1.5"
                          title="Open in Build History"
                        >
                          <span v-if="runNumberForJobId(ev.job_id) != null">#{{ runNumberForJobId(ev.job_id) }}</span>
                          <span>{{ ev.job_status || 'view' }}</span>
                        </RouterLink>
                        <span v-else class="text-muted-foreground">—</span>
                      </td>
                    </tr>
                  </tbody>
                </table>
              </div>
              <div
                v-if="triggerEventsTotal > 0"
                class="px-4 py-3 border-t border-border/30 flex items-center justify-end gap-2 text-xs"
              >
                <button
                  type="button"
                  class="px-2 py-1 rounded-sm hover:bg-muted disabled:opacity-40"
                  :disabled="triggerEventsPage <= 1"
                  @click="triggerEventsPage -= 1; loadTriggerEvents()"
                >Prev</button>
                <span class="tabular-nums text-muted-foreground">{{ triggerEventsPage }} / {{ Math.max(1, Math.ceil(triggerEventsTotal / triggerEventsPageSize)) }}</span>
                <button
                  type="button"
                  class="px-2 py-1 rounded-sm hover:bg-muted disabled:opacity-40"
                  :disabled="triggerEventsPage * triggerEventsPageSize >= triggerEventsTotal"
                  @click="triggerEventsPage += 1; loadTriggerEvents()"
                >Next</button>
              </div>
            </div>
          </div>

        </div>
      </div>
    </template>

    <div v-else class="flex-1 flex items-center justify-center text-sm text-muted-foreground">Pipeline not found</div>

    <PipelineYamlEditorDialog ref="yamlEditorRef" @saved="onYamlSaved" />
  </div>
</template>
