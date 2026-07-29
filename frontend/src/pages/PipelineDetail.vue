<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch, reactive } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import UiButton from '@/components/ui/UiButton.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import { ArrowLeft, Play, Loader2, Plus, Trash2, CheckCircle2, XCircle, Clock, Pencil, X, GripVertical, Code, FileText, ChevronDown, ChevronRight, Copy, Check } from '@lucide/vue'
import { cn } from '@/lib/utils'
import { pipelinesApi, type PipelineInputDef } from '@/api/pipelines'
import { secretsApi } from '@/api/secrets'
import { variablesApi } from '@/api/variables'
import { teamsApi, type Team } from '@/api/teams'
import { jobsApi, type Job, type LogChunk } from '@/api/jobs'
import { parseUtc } from '@/utils/time'
import { type StepDef } from '@/api/pipelines'
import PipelineTopologyGraph from '@/components/flow/PipelineTopologyGraph.vue'
import BuildHistoryCard from '@/components/flow/BuildHistoryCard.vue'
import MultiSelect from '@/components/ui/MultiSelect.vue'
import { createManagedWebSocket, type ManagedWebSocket } from '@/utils/ws'
import { navLog } from '@/utils/navigation'
import { useAuthStore } from '@/stores/auth'

const route = useRoute()
const router = useRouter()
const auth = useAuthStore()
const pipelineId = computed(() => route.params.id as string)

const pipeline = ref<any>(null)

// When route.params.id changes (navigating between pipelines), reload data
watch(pipelineId, async () => {
  const id = pipelineId.value
  if (!id) return
  if (refreshTimer) { clearInterval(refreshTimer); refreshTimer = null }
  pipelineWs?.close()
  pipelineWs = null
  jobs.value = []
  pipeline.value = null
  await loadData()
  startPipelineWebSocket()
})
const jobs = ref<Job[]>([])
const loading = ref(true)
const triggering = ref(false)
const activeTab = computed(() => (route.params.tab as string) || 'stage')
let refreshTimer: ReturnType<typeof setInterval> | null = null
let pipelineWs: ManagedWebSocket | null = null
let alive = true

const showTriggerDialog = ref(false)
const inputPairs = ref<{ key: string; value: string; required?: boolean }[]>([])
const editAvailableSecrets = ref<{ id: string; name: string; owner_group: string }[]>([])
const editAvailableVariables = ref<{ id: string; name: string; value: string }[]>([])
const editAvailableTeams = ref<Team[]>([])

// Pagination
const historyPage = ref(1)
const historyPageSize = 10

// ── Edit Dialog ──
const showEditDialog = ref(false)
const editSaving = ref(false)
const editForm = ref({
  name: '',
  description: '',
  trigger_type: 'manual',
  webhook_path: '',
  cron_expr: '',
  runner_labels: 'linux-x64',
  runner_timeout: '',
  concurrency_group: '',
  owner_group: '',
  team_id: '',
  var_ids: [] as string[],
  secret_ids: [] as string[],
  env: [] as { name: string; value: string }[],
  steps: [] as StepDef[],
  input_defs: [] as PipelineInputDef[],
})
const expandedEditSteps = reactive(new Set<number>())

function toggleEditStep(idx: number) {
  if (expandedEditSteps.has(idx)) expandedEditSteps.delete(idx)
  else expandedEditSteps.add(idx)
}

const editFormHasAdvanced = computed(() => {
  return !!(editForm.value.runner_timeout.trim() || editForm.value.concurrency_group.trim() ||
    editForm.value.var_ids.length > 0 || editForm.value.secret_ids.length > 0 ||
    editForm.value.env.length > 0 || editForm.value.input_defs.length > 0)
})

type VolumeRow = { host: string; container: string }

function parseVolumesToRows(raw: any): VolumeRow[] {
  if (Array.isArray(raw)) {
    return raw.map((v: string) => {
      const [host, container] = v.split(':')
      return { host: host?.trim() || '', container: container?.trim() || '' }
    }).filter(r => r.host || r.container)
  }
  if (typeof raw === 'string' && raw.trim()) {
    return raw.split(',').map(p => p.trim()).filter(Boolean).map((v: string) => {
      const [host, container] = v.split(':')
      return { host: host?.trim() || '', container: container?.trim() || '' }
    }).filter(r => r.host || r.container)
  }
  return []
}

function rowsToVolumesArray(rows: VolumeRow[]): string[] | undefined {
  const arr = rows
    .filter(r => r.host.trim() && r.container.trim())
    .map(r => `${r.host.trim()}:${r.container.trim()}`)
  return arr.length > 0 ? arr : undefined
}

function addEditStep() {
  const idx = editForm.value.steps.length
  editForm.value.steps.push({ name: '', run: '', uses: 'ssh', volumes: [] as any, secrets: [] })
  expandedEditSteps.add(idx)
}

function removeEditStep(idx: number) {
  editForm.value.steps.splice(idx, 1)
  expandedEditSteps.delete(idx)
  const next = new Set<number>()
  for (const e of expandedEditSteps) {
    if (e > idx) next.add(e - 1)
    else if (e < idx) next.add(e)
  }
  expandedEditSteps.clear()
  for (const n of next) expandedEditSteps.add(n)
}

function addEditInputDef() {
  editForm.value.input_defs.push({ name: '', description: '', required: false, default: '' })
}

function removeEditInputDef(idx: number) {
  editForm.value.input_defs.splice(idx, 1)
}

function addEditEnv() {
  editForm.value.env.push({ name: '', value: '' })
}

function removeEditEnv(idx: number) {
  editForm.value.env.splice(idx, 1)
}

const editCopiedPath = ref(false)
function copyWebhookUrl(path: string) {
  if (!path) return
  const fullUrl = `${window.location.origin}/api/v1/hooks/${path}`
  navigator.clipboard.writeText(fullUrl)
  editCopiedPath.value = true
  setTimeout(() => { editCopiedPath.value = false }, 2000)
}

async function openEditDialog() {
  if (!pipeline.value) return
  if (pipeline.value.source === 'yaml') {
    alert('This pipeline is managed by a YAML file. Edit the source file directly — changes will be picked up automatically on the next scan.')
    return
  }
  const p = pipeline.value
  const tasks = p.tasks || []
  editForm.value = {
    name: p.name || '',
    description: p.description || '',
    trigger_type: p.trigger_type || 'manual',
    webhook_path: p.webhook_path || '',
    cron_expr: p.cron_expr || '',
    runner_labels: (p.runner_labels || []).join(', '),
    runner_timeout: p.timeout_seconds ? `${p.timeout_seconds}s` : '',
    concurrency_group: p.concurrency_group || '',
    owner_group: p.owner_group || '',
    team_id: p.team_id || '',
    var_ids: p.var_ids || [],
    secret_ids: p.secret_ids || [],
    env: [],
      steps: tasks.map((t: any) => ({
        name: t.name || '',
        run: t.command_template || '',
        uses: t.exec_type && t.exec_type !== 'local' ? t.exec_type : undefined,
        image: t.image || undefined,
        volumes: parseVolumesToRows(t.volumes) as any,
        secrets: t.step_secrets ? t.step_secrets.split(',').map((s: string) => s.trim()).filter(Boolean) : [],
        host: t.host_id || undefined,
      host_labels: t.host_labels || undefined,
      dir: t.work_dir || undefined,
      timeout: t.timeout_seconds ? `${t.timeout_seconds}s` : undefined,
      retry: t.retry_count || undefined,
      continue_on_error: t.fail_strategy === 'continue' ? true : undefined,
    })),
    input_defs: (p.input_defs || []).slice(),
  }
  expandedEditSteps.clear()
  for (let i = 0; i < editForm.value.steps.length; i++) expandedEditSteps.add(i)
  showEditDialog.value = true
}

async function handleEditSave() {
  if (!editForm.value.name.trim()) return
  if (editForm.value.steps.length === 0) { alert('Add at least one step'); return }
  if (auth.showTeamPicker && !editForm.value.team_id.trim()) {
    alert('Team is required')
    return
  }
  if (!auth.showTeamPicker && !editForm.value.team_id.trim()) {
    editForm.value.team_id = auth.soleTeamId || editForm.value.team_id
  }
  if (!editForm.value.team_id.trim()) { alert('Team is required'); return }

  editSaving.value = true
  try {
    const labels = editForm.value.runner_labels.split(',').map(s => s.trim()).filter(Boolean)
    let timeout_seconds: number | undefined = undefined
    if (editForm.value.runner_timeout.trim()) {
      const raw = editForm.value.runner_timeout.trim()
      const match = raw.match(/^(\d+)(s|m|h)$/i)
      if (match) {
        const val = parseInt(match[1], 10)
        const unit = match[2].toLowerCase()
        if (unit === 's') timeout_seconds = val
        else if (unit === 'm') timeout_seconds = val * 60
        else if (unit === 'h') timeout_seconds = val * 3600
      }
    }
    const payload = {
      name: editForm.value.name.trim(),
      description: editForm.value.description.trim() || undefined,
      trigger_type: editForm.value.trigger_type,
      webhook_path: editForm.value.trigger_type === 'webhook' ? editForm.value.webhook_path || undefined : undefined,
      cron_expr: editForm.value.trigger_type === 'cron' ? editForm.value.cron_expr || undefined : undefined,
      runner_labels: labels,
      timeout_seconds,
      concurrency_group: editForm.value.concurrency_group.trim() || undefined,
      team_id: editForm.value.team_id,
      var_ids: editForm.value.var_ids.length > 0 ? editForm.value.var_ids : undefined,
      secret_ids: editForm.value.secret_ids.length > 0 ? editForm.value.secret_ids : undefined,
      env: editForm.value.env.filter(e => e.name.trim() && e.value.trim()).length > 0
        ? editForm.value.env.filter(e => e.name.trim() && e.value.trim()).map(e => ({ name: e.name.trim(), value: e.value }))
        : undefined,
      steps: editForm.value.steps.map(s => {
        const rawHost = s.host as string | undefined
        const rawHostLabels = s.host_labels as string | undefined
        const isDocker = s.uses === 'docker'
        const volumesArray = isDocker ? rowsToVolumesArray((s as any).volumes || []) : undefined
        return {
          ...s,
          image: isDocker ? (s.image || '').trim() || undefined : undefined,
          volumes: volumesArray,
          host: isDocker
            ? undefined
            : rawHost
              ? (() => {
                  const hosts = rawHost.split(',').map((h: string) => h.trim()).filter(Boolean)
                  return hosts.length > 1 ? hosts : (hosts[0] || undefined)
                })()
              : undefined,
          host_labels: isDocker
            ? undefined
            : rawHostLabels
              ? (() => {
                  const labels = rawHostLabels.split(',').map((l: string) => l.trim()).filter(Boolean)
                  return labels.length > 1 ? labels : (labels[0] || undefined)
                })()
              : undefined,
        }
      }),
      input_defs: editForm.value.trigger_type === 'manual' && editForm.value.input_defs.length > 0
        ? editForm.value.input_defs
        : undefined,
    }
    await pipelinesApi.update(pipelineId.value, payload)
    showEditDialog.value = false
    await loadData()
  } catch (e) {
    console.error(e)
    alert('Failed to update pipeline')
  } finally {
    editSaving.value = false
  }
}
// ──────────────────────────────

const hasInputDefs = computed(() => {
  const defs: PipelineInputDef[] | undefined = pipeline.value?.input_defs
  return Array.isArray(defs) && defs.length > 0
})

const successCount = computed(() => jobs.value.filter(j => j.status === 'success').length)
const failedCount = computed(() => jobs.value.filter(j => j.status === 'failed').length)
const runningCount = computed(() => jobs.value.filter(j => j.status === 'running').length)
const hasRunningJobs = computed(() => runningCount.value > 0 || jobs.value.some(j => j.status === 'pending'))

const stageViewRuns = computed(() => jobs.value.slice(0, Math.min(jobs.value.length, 8)))
const latestRun = computed(() => jobs.value[0] ?? null)
const hasYamlSource = computed(() => Boolean(pipeline.value?.yaml_source))
const historyTotalPages = computed(() => Math.ceil(jobs.value.length / historyPageSize))
const historyPagedJobs = computed(() => {
  const start = (historyPage.value - 1) * historyPageSize
  return jobs.value.slice(start, start + historyPageSize)
})

// ── History log loading (lazy, on card expand) ──
const historyLogs = ref<Map<string, LogChunk[]>>(new Map())

async function loadHistoryLogs(jobId: string) {
  if (!historyLogs.value.has(jobId)) {
    try {
      const logs = await jobsApi.logs(jobId)
      historyLogs.value.set(jobId, logs)
    } catch { /* ignore */ }
  }
}

function getHistoryLogs(jobId: string): LogChunk[] {
  return historyLogs.value.get(jobId) || []
}

function runNum(job: Job, paged: Job[]) {
  // Show the global run number (1-based from oldest)
  const idx = jobs.value.findIndex(j => j.id === job.id)
  return idx >= 0 ? jobs.value.length - idx : 0
}

function historyGo(page: number) {
  historyPage.value = Math.max(1, Math.min(page, historyTotalPages.value))
}

/** Returns the raw status string (e.g. 'success', 'running'), not a CSS class. */
function getStepStatus(stepIndex: number, stepStatuses: any[] | null): string {
  if (!stepStatuses) return 'pending'
  const found = stepStatuses.find((s: any) => Number(s.step_order) === stepIndex)
  return found?.status || 'pending'
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
  if (!ts) return '—'
  return ts.slice(5, 19)
}

onMounted(async () => {
  await loadData()
  startPipelineWebSocket()
  try {
    editAvailableSecrets.value = await secretsApi.list()
  } catch { /* ignore */ }
  try {
    editAvailableVariables.value = await variablesApi.list()
  } catch { /* ignore */ }
  try {
    editAvailableTeams.value = await teamsApi.list()
  } catch { /* ignore */ }
})

const editVariableOptions = computed(() =>
  editAvailableVariables.value.map(v => ({ id: v.id, label: v.name }))
)
const editSecretOptions = computed(() =>
  editAvailableSecrets.value.map(s => ({
    id: s.id,
    label: s.owner_group ? `${s.name} · ${s.owner_group}` : s.name,
  }))
)

/** Step-level secrets are stored by name (not id). */
const editStepSecretOptions = computed(() =>
  editAvailableSecrets.value.map(s => ({
    id: s.name,
    label: s.owner_group ? `${s.name} · ${s.owner_group}` : s.name,
  }))
)

function teamLabel(teamId: string | null | undefined): string {
  if (!teamId) return '—'
  const t = editAvailableTeams.value.find(x => x.id === teamId)
  return t ? `${t.name} (${t.slug})` : teamId
}

onUnmounted(() => {
  alive = false
  if (refreshTimer) clearInterval(refreshTimer)
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

function openTriggerDialog() {
  if (!pipeline.value) return
  const defs: PipelineInputDef[] = pipeline.value.input_defs || []
  if (defs.length === 0) {
    // No dynamic params — trigger directly
    doDirectTrigger()
    return
  }
  inputPairs.value = defs.map(d => ({ key: d.name, value: d.default || '', required: d.required }))
  showTriggerDialog.value = true
}

async function doDirectTrigger() {
  if (!pipeline.value || triggering.value) return
  triggering.value = true
  try {
    const result = await pipelinesApi.trigger(pipelineId.value, {})
    router.push(`/jobs/${result.job_id}`)
  } catch (e) {
    console.error(e)
  } finally {
    triggering.value = false
  }
}

function navigateTab(tab: string) {
  router.push({ name: 'pipeline-detail', params: { id: pipelineId.value, tab } })
}

function addInputPair() { inputPairs.value.push({ key: '', value: '' }) }
function removeInputPair(idx: number) {
  if (inputPairs.value.length <= 1) return
  inputPairs.value.splice(idx, 1)
}

async function handleTrigger() {
  if (!pipeline.value || triggering.value) return
  triggering.value = true
  try {
    const inputs: Record<string, string> = {}
    for (const pair of inputPairs.value) {
      if (pair.key.trim()) inputs[pair.key.trim()] = pair.value
    }
    const result = await pipelinesApi.trigger(pipelineId.value, inputs)
    showTriggerDialog.value = false
    router.push(`/jobs/${result.job_id}`)
  } catch (e) {
    console.error(e)
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
          class="inline-flex items-center gap-1 text-[13px] text-muted-foreground hover:text-foreground transition-colors shrink-0"
          @click="router.push('/pipelines')"
        >
          <ArrowLeft class="h-3.5 w-3.5" />
        </button>
        <div class="h-5 w-px bg-border/40 shrink-0" />
        <h1 class="text-base font-semibold tracking-tight truncate">{{ pipeline?.name || 'Loading…' }}</h1>
        <StatusBadge v-if="pipeline" :status="pipeline.is_active ? 'online' : 'offline'" />
      </div>
      <UiButton size="sm" @click="openTriggerDialog" :disabled="!pipeline || triggering" class="shrink-0">
        <Loader2 v-if="triggering" class="h-4 w-4 animate-spin" />
        <Play v-else class="h-4 w-4" />
        {{ triggering ? 'Running…' : 'Build Now' }}
      </UiButton>
    </div>

    <!-- Trigger Dialog -->
    <Teleport to="body">
      <div v-if="showTriggerDialog" class="fixed inset-0 z-50 flex items-center justify-center">
          <div class="absolute inset-0 bg-black/50" @click="showTriggerDialog = false" />
        <div class="relative z-10 w-full max-w-md rounded-xl border border-border bg-background p-6 shadow-2xl">
          <h2 class="text-base font-semibold mb-1">Trigger Pipeline</h2>
          <p class="text-sm text-muted-foreground mb-4">
            Keys available as <code class="mono-chip">$INPUT_&lt;KEY&gt;</code> in step commands.
          </p>
          <div class="space-y-2 mb-4">
            <p v-if="!hasInputDefs" class="text-[11px] text-muted-foreground mb-2">Ad-hoc mode — no params defined in the pipeline.</p>
            <div v-for="(pair, idx) in inputPairs" :key="idx" class="flex items-center gap-2">
              <div v-if="hasInputDefs" class="flex-1 rounded-md border border-border/40 bg-muted/30 px-3 py-2 text-sm font-mono text-muted-foreground select-none flex items-center gap-1">
                {{ pair.key }}
                <span v-if="pair.required" class="text-red-400 text-[13px] font-sans leading-none">*</span>
                <span v-else class="text-[10px] text-muted-foreground/50 font-sans">(optional)</span>
              </div>
              <input v-else v-model="pair.key" placeholder="key" class="flex-1 rounded-md border border-border bg-muted/50 px-3 py-2 text-sm" :disabled="triggering" />
              <input v-model="pair.value" :placeholder="pair.required ? 'required' : 'enter value (leave empty for default)'" class="flex-[2] rounded-md border border-border bg-muted/50 px-3 py-2 text-sm" :disabled="triggering" />
              <button v-if="!hasInputDefs" @click="removeInputPair(idx)" class="shrink-0 rounded-md p-2 text-muted-foreground hover:bg-muted hover:text-destructive transition-colors" :disabled="triggering || inputPairs.length <= 1"><Trash2 class="h-4 w-4" /></button>
            </div>
          </div>
          <button v-if="!hasInputDefs" @click="addInputPair" class="flex items-center gap-1 text-xs text-primary hover:text-primary/80 transition-colors mb-6" :disabled="triggering"><Plus class="h-3.5 w-3.5" /> Add parameter</button>
          <div class="flex items-center justify-end gap-3">
            <UiButton variant="outline" size="sm" @click="showTriggerDialog = false">Cancel</UiButton>
            <UiButton size="sm" @click="handleTrigger" :disabled="triggering"><Loader2 v-if="triggering" class="mr-1.5 h-4 w-4 animate-spin" /><Play v-else class="mr-1.5 h-4 w-4" /> Run</UiButton>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- Edit Dialog -->
    <Teleport to="body">
      <div
        v-if="showEditDialog"
        class="fixed inset-0 z-50 flex items-start justify-center pt-[8vh]"
        @click.self="showEditDialog = false"
      >
        <div class="soft-panel w-full max-w-xl p-6 space-y-5 max-h-[85vh] overflow-y-auto">
          <div class="flex items-center justify-between">
            <h2 class="text-base font-semibold tracking-tight">Edit Pipeline</h2>
            <button
              type="button"
              class="inline-flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground"
              @click="showEditDialog = false"
            >
              <X class="h-4 w-4" />
            </button>
          </div>

          <div class="space-y-4">
            <div>
              <label class="type-label block mb-1.5">Name <span class="text-red-500">*</span></label>
              <input
                v-model="editForm.name"
                type="text"
                placeholder="e.g. hello-world"
                class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
              />
            </div>
            <div>
              <label class="type-label block mb-1.5">Description</label>
              <input
                v-model="editForm.description"
                type="text"
                placeholder="Optional description"
                class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
              />
            </div>
            <div class="grid grid-cols-2 gap-4">
              <div>
                <label class="type-label block mb-1.5">Trigger</label>
                <select
                  v-model="editForm.trigger_type"
                  class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                >
                  <option value="manual">Manual</option>
                  <option value="webhook">Webhook</option>
                  <option value="cron">Cron</option>
                </select>
              </div>
              <div>
                <label class="type-label block mb-1.5">Runner Labels</label>
                <input
                  v-model="editForm.runner_labels"
                  type="text"
                  placeholder="linux-x64, docker"
                  class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                />
              </div>
            </div>

            <div v-if="editForm.trigger_type === 'webhook'" class="rounded-xl border border-border/70 bg-muted/20 p-4 space-y-3">
              <div class="flex items-center gap-2">
                <Code class="h-4 w-4 text-muted-foreground" />
                <span class="text-sm font-medium">Webhook Configuration</span>
              </div>
              <div>
                <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">URL Path</label>
                <div class="flex items-center gap-2">
                  <div class="flex items-center gap-1 rounded-lg border border-border bg-muted/50 px-3 py-1.5 text-sm font-mono flex-1 min-w-0">
                    <span class="text-muted-foreground shrink-0">/api/v1/hooks/</span>
                    <span class="text-foreground truncate">{{ editForm.webhook_path || '—' }}</span>
                  </div>
                  <button
                    v-if="editForm.webhook_path"
                    type="button"
                    class="shrink-0 inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
                    title="Copy full URL"
                    @click="copyWebhookUrl(editForm.webhook_path)"
                  >
                    <Check v-if="editCopiedPath" class="h-3.5 w-3.5 text-emerald-500" />
                    <Copy v-else class="h-3.5 w-3.5" />
                  </button>
                </div>
              </div>
            </div>

            <div v-if="editForm.trigger_type === 'cron'" class="rounded-xl border border-border/70 bg-muted/20 p-4 space-y-3">
              <div class="flex items-center gap-2">
                <Code class="h-4 w-4 text-muted-foreground" />
                <span class="text-sm font-medium">Cron Configuration</span>
              </div>
              <div>
                <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Schedule (cron expression)</label>
                <input
                  v-model="editForm.cron_expr"
                  type="text"
                  placeholder="0 2 * * *"
                  class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-sm font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                />
              </div>
            </div>

            <div v-if="auth.showTeamPicker">
              <label class="type-label block mb-1.5">Team <span class="text-red-500">*</span></label>
              <select
                v-model="editForm.team_id"
                class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
              >
                <option value="" disabled>Select a team…</option>
                <option v-for="t in editAvailableTeams" :key="t.id" :value="t.id">
                  {{ t.name }} ({{ t.slug }})
                </option>
              </select>
              <p class="mt-1 text-[10px] text-muted-foreground">
                Same-team secrets from Infrastructure → Secrets are auto-injected. Use Authorized Secrets for cross-team bindings.
              </p>
            </div>

            <!-- Steps editor -->
            <div>
              <div class="flex items-center justify-between mb-1.5">
                <span class="type-label">Steps <span class="text-red-500">*</span></span>
                <span class="text-xs text-muted-foreground font-mono">{{ editForm.steps.length }} step(s)</span>
              </div>

              <button
                v-if="editForm.steps.length === 0"
                type="button"
                class="w-full inline-flex items-center justify-center gap-2 rounded-xl border border-dashed border-border px-3 py-2.5 text-[13px] text-muted-foreground hover:border-foreground/25 hover:bg-muted/20 hover:text-foreground transition-colors"
                @click="addEditStep"
              >
                <span class="inline-flex items-center gap-1.5 font-medium">
                  <Plus class="h-3.5 w-3.5" />
                  Add Step
                </span>
                <span class="text-border">·</span>
                <span class="text-[12px] font-normal text-muted-foreground/70">No steps defined yet</span>
              </button>

              <template v-else>
              <div class="space-y-2 mb-3">
                <div
                  v-for="(step, idx) in editForm.steps"
                  :key="idx"
                  class="rounded-xl border border-border/70 overflow-hidden transition-colors"
                  :class="expandedEditSteps.has(idx) ? 'bg-card' : 'bg-muted/30'"
                >
                  <button
                    type="button"
                    class="w-full flex items-center gap-2.5 px-3 py-2.5 text-left hover:bg-muted/30 transition-colors"
                    @click="toggleEditStep(idx)"
                  >
                    <span class="inline-flex items-center justify-center h-6 w-6 rounded-md bg-muted font-mono text-xs text-muted-foreground shrink-0">{{ idx + 1 }}</span>
                    <div class="flex-1 min-w-0">
                      <div v-if="step.name" class="text-sm font-medium truncate">{{ step.name }}</div>
                      <div v-if="step.run" class="text-xs font-mono text-muted-foreground truncate mt-0.5">{{ step.run.slice(0, 60) }}</div>
                      <div v-if="!step.name && !step.run" class="text-xs text-muted-foreground italic">Empty step</div>
                    </div>
                    <div class="flex items-center gap-1 shrink-0">
                      <span v-if="step.uses" class="mono-chip text-[10px]">{{ step.uses }}</span>
                      <span v-if="step.continue_on_error" class="mono-chip text-[10px] text-amber-600 bg-amber-500/10">no-fail</span>
                      <button
                        type="button"
                        class="inline-flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                        @click.stop="removeEditStep(idx)"
                      >
                        <X class="h-3.5 w-3.5" />
                      </button>
                      <ChevronDown v-if="expandedEditSteps.has(idx)" class="h-4 w-4 text-muted-foreground" />
                      <ChevronRight v-else class="h-4 w-4 text-muted-foreground" />
                    </div>
                  </button>

                  <!-- Step body (expanded) -->
                  <div v-if="expandedEditSteps.has(idx)" class="px-4 pb-4 space-y-3 border-t border-border/40 pt-3">
                    <div class="grid grid-cols-5 gap-3">
                      <div class="col-span-3">
                        <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Step Name <span class="text-red-500">*</span></label>
                        <input
                          v-model="step.name"
                          type="text"
                          placeholder="e.g. Install dependencies"
                          class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                        />
                      </div>
                      <div class="col-span-2">
                        <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Engine</label>
                        <select
                          v-model="step.uses"
                          class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                        >
                          <option value="ssh">SSH</option>
                          <option value="">Local</option>
                          <option value="docker">Docker</option>
                        </select>
                      </div>
                    </div>

                    <div v-if="step.uses === 'docker'">
                      <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Image <span class="text-red-500">*</span></label>
                      <input
                        v-model="step.image"
                        type="text"
                        placeholder="e.g. alpine:3.20"
                        class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-sm font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                      />
                    </div>

                                          <div v-if="step.uses === 'docker'">
                        <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">
                          Volumes
                        </label>
                        <div v-if="((step as any).volumes || []).length > 0" class="rounded-lg border border-border overflow-hidden mb-1.5">
                          <div class="grid grid-cols-[1fr_1fr_28px] gap-0 text-[11px] font-medium uppercase tracking-wider text-muted-foreground bg-muted/30 border-b border-border px-3 py-1.5">
                            <span>Host Path</span>
                            <span>Container Path</span>
                            <span />
                          </div>
                          <div
                            v-for="(vol, volIdx) in (step as any).volumes"
                            :key="volIdx"
                            class="grid grid-cols-[1fr_1fr_28px] gap-0 border-b border-border/40 last:border-b-0"
                          >
                            <input
                              v-model="vol.host"
                              type="text"
                              placeholder="/data/logs"
                              class="w-full bg-transparent px-3 py-1.5 text-xs font-mono outline-none border-r border-border/30"
                            />
                            <input
                              v-model="vol.container"
                              type="text"
                              placeholder="/var/log/app"
                              class="w-full bg-transparent px-3 py-1.5 text-xs font-mono outline-none border-r border-border/30"
                            />
                            <button
                              type="button"
                              class="flex items-center justify-center text-muted-foreground/50 hover:text-red-500 transition-colors"
                              title="Remove volume"
                              @click="(step as any).volumes.splice(volIdx, 1)"
                            >
                              <svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/></svg>
                            </button>
                          </div>
                        </div>
                        <button
                          type="button"
                          class="flex items-center gap-1 px-3 py-1.5 text-[11px] font-medium text-muted-foreground hover:text-foreground bg-muted/20 hover:bg-muted/40 rounded-lg border border-dashed border-border/60 transition-colors"
                          @click="
                            if (!(step as any).volumes) (step as any).volumes = [];
                            (step as any).volumes.push({ host: '', container: '' })
                          "
                        >
                          <svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/><path d="M12 5v14"/></svg>
                          Add Volume
                        </button>
                        <p class="text-[10px] text-muted-foreground/70 mt-1">Map host paths into the container. All host paths must exist on the runner.</p>
                      </div>

                    <div>
                      <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Command</label>
                      <textarea
                        v-model="step.run"
                        rows="4"
                        spellcheck="false"
                        placeholder="echo Hello&#10;npm run build&#10;cargo test --release"
                        class="w-full rounded-lg border border-border bg-input px-3 py-2 text-sm font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15 resize-y"
                      />
                    </div>

                      <div class="grid grid-cols-3 gap-3">
                        <div>
                          <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Work Dir</label>
                          <input
                            v-model="step.dir"
                            type="text"
                            placeholder="e.g. ./frontend"
                            class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-xs outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                          />
                        </div>
                        <div>
                          <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Timeout</label>
                          <input
                            v-model="step.timeout"
                            type="text"
                            placeholder="e.g. 10m"
                            class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-xs outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                          />
                        </div>
                        <div>
                          <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Retry</label>
                          <input
                            v-model.number="step.retry"
                            type="number"
                            min="0"
                            max="5"
                            placeholder="0"
                            class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-xs outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                          />
                        </div>
                      </div>

                      <div class="mt-3">
                        <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Step Secrets</label>
                        <MultiSelect
                          v-model="(step.secrets as string[])"
                          :options="editStepSecretOptions"
                          placeholder="Inherit from pipeline..."
                        />
                        <p class="text-[10px] text-muted-foreground/70 mt-0.5">Authorize secrets for this step only. Injected as env vars at runtime.</p>
                      </div>

                      <div v-if="step.uses === 'ssh'" class="space-y-3">
                        <div class="grid grid-cols-2 gap-3">
                          <div>
                            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">SSH Host <span class="text-red-500">*</span></label>
                            <input
                              v-model="step.host"
                              type="text"
                              placeholder="e.g. prod-server-01, prod-server-02"
                              class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-xs outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                            />
                          </div>
                          <div>
                            <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Host Labels</label>
                            <input
                              v-model="step.host_labels"
                              type="text"
                              placeholder="e.g. prod, web"
                              class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-xs outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                            />
                          </div>
                        </div>
                        <p class="text-[10px] text-muted-foreground">Host labels match hosts by their labels column. Use commas for multiple. Combined with SSH Host above (union).</p>
                      </div>
                      <label class="flex items-center gap-2 cursor-pointer select-none">
                        <input
                          type="checkbox"
                          v-model="step.continue_on_error"
                          class="h-4 w-4 rounded border-border accent-primary"
                        />
                        <span class="text-xs text-muted-foreground">Continue on error (next steps will still run)</span>
                      </label>
                  </div>
                </div>
              </div>

              <button
                type="button"
                class="w-full inline-flex items-center justify-center gap-1.5 rounded-xl border border-border px-4 py-2 text-[13px] font-medium text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
                @click="addEditStep"
              >
                <Plus class="h-3.5 w-3.5" />
                Add Step
              </button>
              </template>
            </div>
            <!-- Advanced section -->
            <details class="group/adv rounded-xl border border-dashed border-border/80 bg-muted/10 px-3 py-2.5" :open="editFormHasAdvanced">
              <summary class="flex items-center gap-3 cursor-pointer list-none select-none py-0.5 [&::-webkit-details-marker]:hidden">
                <span class="h-px flex-1 bg-border/80 transition-colors duration-300 group-hover/adv:bg-foreground/20" />
                <span class="inline-flex items-center gap-1.5 shrink-0 text-[11px] font-medium uppercase tracking-[0.14em] text-muted-foreground transition-colors duration-300 group-hover/adv:text-foreground">
                  Advanced
                  <ChevronDown class="h-3.5 w-3.5 transition-transform duration-300 ease-out group-open/adv:rotate-180" />
                  <span v-if="editFormHasAdvanced" class="text-[10px] font-normal normal-case tracking-normal text-muted-foreground/60">
                    ({{ [editForm.runner_timeout && 1, editForm.concurrency_group && 1, editForm.var_ids.length > 0 && 1, editForm.secret_ids.length > 0 && 1, editForm.env.length > 0 && 1, editForm.input_defs.length > 0 && 1].filter(Boolean).length }} configured)
                  </span>
                </span>
                <span class="h-px flex-1 bg-border/80 transition-colors duration-300 group-hover/adv:bg-foreground/20" />
              </summary>
              <div class="grid grid-rows-[0fr] transition-[grid-template-rows] duration-300 ease-out group-open/adv:grid-rows-[1fr]">
                <div class="overflow-hidden">
                  <div class="mt-3 space-y-4 pt-1">
              <div class="grid grid-cols-2 gap-4">
                <div>
                  <label class="type-label block mb-1.5">Runner Timeout</label>
                  <input
                    v-model="editForm.runner_timeout"
                    type="text"
                    placeholder="e.g. 60m"
                    class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                  />
                </div>
                <div>
                  <label class="type-label block mb-1.5">Concurrency Group</label>
                  <input
                    v-model="editForm.concurrency_group"
                    type="text"
                    placeholder="e.g. prod"
                    class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                  />
                </div>
              </div>

              <div class="grid grid-cols-2 gap-4">
                <div>
                  <label class="type-label block mb-1.5">Variables</label>
                  <MultiSelect
                    v-model="editForm.var_ids"
                    :options="editVariableOptions"
                    placeholder="Select platform variables..."
                  />
                  <p class="mt-1 text-[10px] text-muted-foreground">
                    Bind platform variables (Infrastructure → Variables). Available as <code class="font-mono">$NAME</code> in every step.
                  </p>
                </div>
                <div>
                  <label class="type-label block mb-1.5">Authorized Secrets</label>
                  <MultiSelect
                    v-model="editForm.secret_ids"
                    :options="editSecretOptions"
                    placeholder="Select secrets..."
                  />
                  <p class="mt-1 text-[10px] text-muted-foreground">
                    Explicit binding via <code class="font-mono">pipeline_secrets</code>. Use for cross-group or empty-group secrets.
                  </p>
                </div>
              </div>

              <!-- Env (pipeline-level key-value pairs, user-defined) -->
              <div class="rounded-xl border border-border/70 bg-muted/20 p-4 space-y-3">
                <div class="flex items-center justify-between">
                  <div class="flex items-center gap-2">
                    <GripVertical class="h-4 w-4 text-muted-foreground" />
                    <span class="text-sm font-medium">Env</span>
                    <span class="text-[11px] text-muted-foreground">(user-defined, only for this pipeline)</span>
                  </div>
                  <button
                    type="button"
                    class="inline-flex items-center gap-1 text-[11px] font-medium text-primary hover:text-primary/80 transition-colors"
                    @click="addEditEnv"
                  >
                    <Plus class="h-3 w-3" /> Add
                  </button>
                </div>
                <div v-if="editForm.env.length === 0" class="text-[11px] text-muted-foreground">
                  No custom env vars defined. Add key-value pairs specific to this pipeline.
                </div>
                <div v-for="(pair, ei) in editForm.env" :key="ei" class="flex items-center gap-3">
                  <input
                    v-model="editForm.env[ei].name"
                    type="text"
                    placeholder="KEY"
                    class="flex-1 rounded-lg border border-border bg-input px-2.5 py-1.5 text-xs font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                  />
                  <span class="text-[10px] text-muted-foreground">=</span>
                  <input
                    v-model="editForm.env[ei].value"
                    type="text"
                    placeholder="value"
                    class="flex-1 rounded-lg border border-border bg-input px-2.5 py-1.5 text-xs font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                  />
                  <button type="button" class="text-muted-foreground hover:text-destructive transition-colors" @click="removeEditEnv(ei)">
                    <X class="h-3.5 w-3.5" />
                  </button>
                </div>
              </div>


              <!-- Input parameters (manual trigger only) -->
              <div v-if="editForm.trigger_type === 'manual'" class="rounded-xl border border-border/70 bg-muted/20 p-4 space-y-3">
                <div class="flex items-center justify-between">
                  <div class="flex items-center gap-2">
                    <GripVertical class="h-4 w-4 text-muted-foreground" />
                    <span class="text-sm font-medium">Params</span>
                    <span class="text-[11px] text-muted-foreground">(optional)</span>
                  </div>
                  <button
                    type="button"
                    class="inline-flex items-center gap-1 text-[11px] font-medium text-primary hover:text-primary/80 transition-colors"
                    @click="addEditInputDef"
                  >
                    <Plus class="h-3 w-3" /> Add
                  </button>
                </div>

                <div v-if="editForm.input_defs.length === 0" class="text-[11px] text-muted-foreground">
                  No input parameters defined.
                </div>

                <div
                  v-for="(inp, idx) in editForm.input_defs"
                  :key="idx"
                  class="rounded-lg border border-border/50 bg-background px-3 py-2.5"
                >
                  <div class="flex items-start gap-2 mb-2">
                    <input
                      v-model="inp.name"
                      type="text"
                      placeholder="param name (e.g. version)"
                      class="flex-1 rounded-md border border-border bg-muted/50 px-2.5 py-1.5 text-sm font-mono outline-none transition-colors focus:border-ring/40"
                    />
                    <button
                      type="button"
                      class="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:text-destructive transition-colors shrink-0"
                      @click="removeEditInputDef(idx)"
                    >
                      <Trash2 class="h-3.5 w-3.5" />
                    </button>
                  </div>
                  <div class="grid grid-cols-1 sm:grid-cols-3 gap-2">
                    <input
                      v-model="inp.description"
                      type="text"
                      placeholder="Description"
                      class="rounded-md border border-border bg-muted/50 px-2.5 py-1.5 text-xs outline-none transition-colors focus:border-ring/40"
                    />
                    <input
                      v-model="inp.default"
                      type="text"
                      placeholder="Default value"
                      class="rounded-md border border-border bg-muted/50 px-2.5 py-1.5 text-xs font-mono outline-none transition-colors focus:border-ring/40"
                    />
                    <label class="flex items-center gap-1.5 text-xs text-muted-foreground cursor-pointer">
                      <input v-model="inp.required" type="checkbox" class="rounded border-border" />
                      Required
                    </label>
                  </div>
                </div>
              </div>

                  </div>
                </div>
              </div>
            </details>

          </div>

          <div class="flex justify-end gap-2 pt-1">
            <button
              type="button"
              class="inline-flex items-center justify-center rounded-xl border border-border px-4 py-2 text-[13px] font-medium text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
              @click="showEditDialog = false"
            >Cancel</button>
            <button
              :disabled="editSaving || !editForm.name.trim() || editForm.steps.length === 0"
              class="inline-flex items-center justify-center rounded-xl bg-primary px-4 py-2 text-[13px] font-medium text-primary-foreground hover:bg-primary/90 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              @click="handleEditSave"
            >
              {{ editSaving ? 'Saving...' : 'Update Pipeline' }}
            </button>
          </div>
        </div>
      </div>
    </Teleport>

    <div v-if="loading" class="flex-1 flex items-center justify-center text-sm text-muted-foreground">Loading…</div>

    <template v-else-if="pipeline">
      <div class="flex gap-0 flex-1 min-h-0 bg-muted/50 rounded-2xl overflow-hidden">
        <!-- ====== Left Sidebar ====== -->
        <div class="w-[16rem] shrink-0 flex flex-col overflow-y-auto p-4">
          <div class="space-y-0.5 mt-1">
            <button
              class="w-full text-left px-3 py-2 rounded-lg text-[13px] transition-colors"
              :class="activeTab === 'stage' ? 'bg-card shadow-sm font-medium text-foreground' : 'text-muted-foreground hover:text-foreground hover:bg-card/50'"
              @click="navigateTab('stage')"
            >
              Stage View
            </button>
            <button
              class="w-full text-left px-3 py-2 rounded-lg text-[13px] transition-colors flex items-center justify-between"
              :class="activeTab === 'history' ? 'bg-card shadow-sm font-medium text-foreground' : 'text-muted-foreground hover:text-foreground hover:bg-card/50'"
              @click="navigateTab('history')"
            >
              <span>Build History</span>
              <span v-if="jobs.length > 0" class="text-[11px] tabular-nums">{{ jobs.length }}</span>
            </button>
            <button
              class="w-full text-left px-3 py-2 rounded-lg text-[13px] transition-colors"
              :class="activeTab === 'definition' ? 'bg-card shadow-sm font-medium text-foreground' : 'text-muted-foreground hover:text-foreground hover:bg-card/50'"
              @click="navigateTab('definition')"
            >
              Definition
            </button>
          </div>
          <div class="mt-4 px-3 space-y-2 text-[11px] text-muted-foreground">
            <p v-if="pipeline.description" class="leading-relaxed">{{ pipeline.description }}</p>
            <div class="space-y-1">
              <div class="flex justify-between">
                <span>Trigger</span>
                <span class="font-medium text-foreground">{{ pipeline.trigger_type }}</span>
              </div>
              <div class="flex justify-between">
                <span>Timeout</span>
                <span class="font-mono font-medium text-foreground">{{ pipeline.timeout_seconds }}s</span>
              </div>
              <div v-if="pipeline.runner_labels?.length" class="flex justify-between">
                <span>Labels</span>
                <span class="font-mono font-medium text-foreground text-right">{{ pipeline.runner_labels.join(', ') }}</span>
              </div>
            </div>
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
              <div class="rounded-xl border border-border/50 bg-card p-5 mb-5 flex items-center gap-6 shadow-sm">
                <div class="flex items-center gap-5">
                  <div class="text-center min-w-[3rem]">
                    <div class="text-xl font-bold tabular-nums text-emerald-500">{{ successCount }}</div>
                    <div class="text-[11px] text-muted-foreground mt-0.5">Passed</div>
                  </div>
                  <div class="text-center min-w-[3rem]">
                    <div class="text-xl font-bold tabular-nums text-red-500">{{ failedCount }}</div>
                    <div class="text-[11px] text-muted-foreground mt-0.5">Failed</div>
                  </div>
                  <div class="text-center min-w-[3rem]">
                    <div class="text-xl font-bold tabular-nums text-foreground">{{ jobs.length }}</div>
                    <div class="text-[11px] text-muted-foreground mt-0.5">Total</div>
                  </div>
                </div>
                <div class="w-px h-10 bg-border/30" />
                <div v-if="latestRun" class="min-w-0">
                  <div class="text-[11px] text-muted-foreground mb-1.5">Last build</div>
                  <div class="flex items-center gap-2 text-[13px]">
                    <span class="font-mono font-semibold text-primary tabular-nums">#{{ runNum(latestRun, []) }}</span>
                    <span
                      :class="cn(
                        'inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px] font-medium',
                        latestRun.status === 'success' ? 'bg-emerald-500/10 text-emerald-600' :
                        latestRun.status === 'failed' ? 'bg-red-500/10 text-red-500' :
                        latestRun.status === 'running' ? 'bg-amber-500/10 text-amber-600' :
                        'bg-slate-500/10 text-slate-500'
                      )"
                    >
                      <span :class="cn('w-1.5 h-1.5 rounded-full', latestRun.status === 'success' ? 'bg-emerald-500' : latestRun.status === 'failed' ? 'bg-red-500' : latestRun.status === 'running' ? 'bg-amber-400 animate-pulse' : 'bg-slate-400')" />
                      {{ latestRun.status }}
                    </span>
                    <span class="font-mono text-[12px] text-muted-foreground">{{ fmtDuration(latestRun.start_time, latestRun.end_time) }}</span>
                    <span class="text-muted-foreground/40">·</span>
                    <span class="text-[12px] font-mono text-muted-foreground">{{ fmtTime(latestRun.start_time || latestRun.created_at) }}</span>
                  </div>
                </div>
                <div v-else class="text-[12px] text-muted-foreground">No builds yet</div>
              </div>

              <!-- Workflow graph — VueFlow + dagre -->
              <PipelineTopologyGraph
                :tasks="(pipeline.tasks || []) as any[]"
                :step-statuses="latestRun?.step_statuses || null"
                :fmt-duration="(idx: number, ss: any[] | null) => fmtStepDuration(idx, ss)"
                @step-click="(p: { stepIndex: number }) => router.push('/jobs/' + (latestRun?.id || '') + '?step=' + p.stepIndex)"
              />
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
                <div class="text-[13px] font-semibold text-foreground">Build History</div>
                <div class="flex items-center gap-3 ml-auto text-[11px]">
                  <span class="flex items-center gap-1 text-emerald-600"><CheckCircle2 class="h-3 w-3" /><span class="font-semibold tabular-nums">{{ successCount }}</span></span>
                  <span class="flex items-center gap-1 text-red-500"><XCircle class="h-3 w-3" /><span class="font-semibold tabular-nums">{{ failedCount }}</span></span>
                  <span class="text-muted-foreground">{{ jobs.length }} total</span>
                </div>
              </div>

              <div v-if="jobs.length === 0" class="rounded-xl border border-border/50 bg-card shadow-sm flex flex-col items-center justify-center py-20 text-muted-foreground">
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
                  @expand="loadHistoryLogs($event)"
                />

                <!-- Pagination -->
                <div v-if="historyTotalPages > 1" class="flex items-center justify-between px-4 py-2.5 rounded-xl border border-border/50 bg-card">
                  <span class="text-[11px] text-muted-foreground">
                    {{ (historyPage - 1) * historyPageSize + 1 }}–{{ Math.min(historyPage * historyPageSize, jobs.length) }} of {{ jobs.length }}
                  </span>
                  <div class="flex items-center gap-1">
                    <button
                      class="px-2.5 py-1 rounded text-[11px] transition-colors"
                      :class="historyPage === 1 ? 'text-muted-foreground/40 cursor-default' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
                      :disabled="historyPage === 1"
                      @click="historyGo(historyPage - 1)"
                    >Prev</button>
                    <button
                      v-for="p in Math.min(historyTotalPages, 7)"
                      :key="'pg-'+p"
                      class="w-7 h-7 rounded text-[11px] tabular-nums transition-colors"
                      :class="p === historyPage ? 'bg-primary text-primary-foreground font-semibold' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
                      @click="historyGo(p)"
                    >{{ p }}</button>
                    <button
                      class="px-2.5 py-1 rounded text-[11px] transition-colors"
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
            <div class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
              <div class="px-5 py-3 border-b border-border/60 flex items-center justify-between">
                <h2 class="text-[13px] font-semibold tracking-tight">Pipeline Definition</h2>
                <div class="flex items-center gap-2">
                  <span v-if="hasYamlSource" class="text-[11px] text-muted-foreground bg-muted/60 rounded px-2.5 py-0.5">from YAML</span>
                  <button
                    v-if="!hasYamlSource"
                    class="inline-flex items-center gap-1 text-[11px] font-medium text-primary hover:text-primary/80 transition-colors"
                    @click="openEditDialog"
                  >
                    <Pencil class="h-3 w-3" />
                    Edit
                  </button>
                </div>
              </div>

              <!-- ── General ── -->
              <div class="border-b border-border/20">
                <div class="px-5 py-3 bg-muted/10 border-b border-border/20">
                  <span class="text-[12px] font-semibold text-foreground">General</span>
                </div>
                <div class="grid grid-cols-2 gap-y-2 gap-x-6 px-5 py-3 text-[13px]">
                  <div class="flex gap-3">
                    <span class="text-muted-foreground shrink-0 w-[72px]">Name</span>
                    <span class="font-semibold text-foreground">{{ pipeline.name }}</span>
                  </div>
                  <div class="flex gap-3">
                    <span class="text-muted-foreground shrink-0 w-[72px]">Source</span>
                    <span class="mono-chip text-[10px] uppercase">{{ pipeline.source || 'form' }}</span>
                  </div>
                  <div v-if="pipeline.description" class="flex gap-3">
                    <span class="text-muted-foreground shrink-0 w-[72px]">Description</span>
                    <span class="text-foreground text-[12px]">{{ pipeline.description }}</span>
                  </div>
                </div>
              </div>

              <!-- ── Trigger ── -->
              <div class="border-b border-border/20">
                <div class="px-5 py-3 bg-muted/10 border-b border-border/20">
                  <span class="text-[12px] font-semibold text-foreground">Trigger</span>
                </div>
                <div class="grid grid-cols-2 gap-y-2 gap-x-6 px-5 py-3 text-[13px]">
                  <div class="flex gap-3">
                    <span class="text-muted-foreground shrink-0 w-[88px]">Type</span>
                    <span class="mono-chip text-[10px] uppercase">{{ pipeline.trigger_type }}</span>
                  </div>
                  <div v-if="pipeline.webhook_path" class="flex gap-3">
                    <span class="text-muted-foreground shrink-0 w-[88px]">Webhook Path</span>
                    <div class="flex items-center gap-1.5">
                      <code class="mono-chip text-[11px]">{{ pipeline.webhook_path }}</code>
                      <button
                        class="inline-flex h-6 w-6 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
                        title="Copy full URL"
                        @click="copyWebhookUrl(pipeline.webhook_path)"
                      >
                        <Check v-if="editCopiedPath" class="h-3 w-3 text-emerald-500" />
                        <Copy v-else class="h-3 w-3" />
                      </button>
                    </div>
                  </div>
                  <div v-if="pipeline.cron_expr" class="flex gap-3">
                    <span class="text-muted-foreground shrink-0 w-[88px]">Cron</span>
                    <code class="mono-chip text-[11px]">{{ pipeline.cron_expr }}</code>
                  </div>
                </div>
              </div>

              <!-- ── Runner ── -->
              <div class="border-b border-border/20">
                <div class="px-5 py-3 bg-muted/10 border-b border-border/20">
                  <span class="text-[12px] font-semibold text-foreground">Runner</span>
                </div>
                <div class="grid grid-cols-2 gap-y-2 gap-x-6 px-5 py-3 text-[13px]">
                  <div class="flex gap-3">
                    <span class="text-muted-foreground shrink-0 w-[88px]">Labels</span>
                    <div class="flex flex-wrap gap-1">
                      <span v-for="l in (pipeline.runner_labels || [])" :key="l" class="mono-chip text-[10px]">{{ l }}</span>
                      <span v-if="!(pipeline.runner_labels || []).length" class="text-muted-foreground/40 text-xs">—</span>
                    </div>
                  </div>
                  <div class="flex gap-3">
                    <span class="text-muted-foreground shrink-0 w-[88px]">Timeout</span>
                    <span class="font-mono text-[12px]">{{ pipeline.timeout_seconds }}s</span>
                  </div>
                  <div v-if="pipeline.concurrency_group" class="flex gap-3">
                    <span class="text-muted-foreground shrink-0 w-[88px]">Concurrency</span>
                    <code class="mono-chip text-[11px]">{{ pipeline.concurrency_group }}</code>
                  </div>
                  <div v-if="pipeline.team_id" class="flex gap-3">
                    <span class="text-muted-foreground shrink-0 w-[88px]">Team</span>
                    <code class="mono-chip text-[11px]">{{ teamLabel(pipeline.team_id) }}</code>
                  </div>
                  <div v-if="pipeline.owner_group" class="flex gap-3">
                    <span class="text-muted-foreground shrink-0 w-[88px]">Owner Group</span>
                    <code class="mono-chip text-[11px]">{{ pipeline.owner_group }}</code>
                  </div>
                </div>
              </div>

              <!-- ── Parameters ── -->
              <div class="border-b border-border/20">
                <div class="px-5 py-3 bg-muted/10">
                  <span class="text-[12px] font-semibold text-foreground">Parameters</span>
                  <span v-if="hasInputDefs" class="text-[11px] text-muted-foreground ml-2">{{ pipeline.input_defs.length }}</span>
                </div>
                <div v-if="!hasInputDefs" class="px-5 py-4 text-[12px] text-muted-foreground">
                  No parameters defined.
                </div>
                <div v-else class="divide-y divide-border/20">
                  <div
                    v-for="inp in pipeline.input_defs"
                    :key="inp.name"
                    class="px-5 py-3 flex items-center gap-4 text-[13px]"
                  >
                    <span class="font-mono font-semibold text-foreground min-w-[8rem] shrink-0">{{ inp.name }}</span>
                    <span v-if="inp.description" class="text-muted-foreground text-[12px] line-clamp-1">{{ inp.description }}</span>
                    <span v-else class="italic text-[12px] text-muted-foreground/50">no description</span>
                    <span class="flex-1" />
                    <div class="flex items-center gap-2 shrink-0">
                      <span
                        :class="cn(
                          'inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide',
                          inp.required ? 'bg-amber-500/10 text-amber-600' : 'bg-slate-500/10 text-slate-500'
                        )"
                      >{{ inp.required ? 'required' : 'optional' }}</span>
                      <span v-if="inp.default != null" class="text-[11px] text-muted-foreground font-mono">default: {{ inp.default }}</span>
                    </div>
                  </div>
                </div>
              </div>

              <!-- ── Steps ── -->
              <div>
                <div class="px-5 py-3 bg-muted/10 border-b border-border/20">
                  <span class="text-[12px] font-semibold text-foreground">Steps</span>
                  <span v-if="(pipeline.tasks || []).length" class="text-[11px] text-muted-foreground ml-2">{{ pipeline.tasks.length }}</span>
                </div>
                <div v-if="!(pipeline.tasks || []).length" class="px-5 py-4 text-[12px] text-muted-foreground">
                  No steps defined.
                </div>
                <div v-else class="divide-y divide-border/20">
                  <div
                    v-for="(step, idx) in pipeline.tasks"
                    :key="idx"
                    class="px-5 py-4"
                  >
                    <div class="flex items-center gap-2 mb-2">
                      <span class="font-mono text-[11px] font-semibold text-muted-foreground">{{ String(Number(idx) + 1).padStart(2, '0') }}</span>
                      <span class="font-semibold text-foreground text-[13px]">{{ step.name }}</span>
                      <span class="mono-chip text-[10px]">{{ step.exec_type }}</span>
                      <span v-if="step.fail_strategy === 'continue'" class="text-[10px] text-amber-600 bg-amber-500/8 rounded px-1.5 py-0.5">continue-on-error</span>
                    </div>
                    <div class="font-mono text-[12px] text-muted-foreground bg-muted/30 rounded-md px-3 py-2 whitespace-pre-wrap break-all mb-2">{{ step.command_template }}</div>
                    <div class="flex flex-wrap items-center gap-2 text-[11px]">
                      <span v-if="step.work_dir" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground">dir:</span>
                        <code class="mono-chip text-[10px]">{{ step.work_dir }}</code>
                      </span>
                      <span v-if="step.host_id" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground">host:</span>
                        <code class="mono-chip text-[10px]">{{ step.host_id }}</code>
                      </span>
                      <span v-if="step.host_labels" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground">labels:</span>
                        <code class="mono-chip text-[10px]">{{ step.host_labels }}</code>
                      </span>
                      <span v-if="step.timeout_seconds > 0" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground">timeout:</span>
                        <span class="font-mono text-[10px]">{{ step.timeout_seconds }}s</span>
                      </span>
                      <span v-if="step.retry_count > 0" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground">retry:</span>
                        <span class="font-mono text-[10px]">{{ step.retry_count }}</span>
                      </span>
                    </div>
                    <div v-if="step.image || step.volumes" class="flex flex-wrap items-center gap-2 text-[11px] mt-1.5">
                      <span v-if="step.image" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground">image:</span>
                        <code class="mono-chip text-[10px]">{{ step.image }}</code>
                      </span>
                      <span v-if="step.volumes" class="inline-flex items-center gap-1">
                        <span class="text-muted-foreground">volumes:</span>
                        <code class="mono-chip text-[10px]">{{ Array.isArray(step.volumes) ? (step.volumes as string[]).join(', ') : step.volumes }}</code>
                      </span>
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </div>

        </div>
      </div>
    </template>

    <div v-else class="flex-1 flex items-center justify-center text-sm text-muted-foreground">Pipeline not found</div>
  </div>
</template>
