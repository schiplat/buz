<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, reactive } from 'vue'
import { useRouter } from 'vue-router'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import { Plus, X, Play, Trash2, GripVertical, Code, FileText, CheckCircle, AlertTriangle, AlertCircle, ChevronDown, ChevronRight, Loader2, Puzzle, Pencil, Minus, Snowflake, Copy, Check, Clock } from '@lucide/vue'
import { pipelinesApi, type Pipeline, type StepDef, type YamlValidationResult, type PipelineInputDef, type YamlParsedData } from '@/api/pipelines'
import {
  emptyInputDef,
  normalizeInputDefsForForm,
  normalizeInputDefsForSave,
} from '@/utils/pipelineInputs'
import { secretsApi } from '@/api/secrets'
import { variablesApi } from '@/api/variables'
import { teamsApi, type Team } from '@/api/teams'
import { jobsApi, type Job } from '@/api/jobs'
import { cn } from '@/lib/utils'
import { parseUtc } from '@/utils/time'
import { resolveNamesToIds } from '@/utils/yamlImportResolve'
import TablePagination from '@/components/ui/TablePagination.vue'
import MultiSelect from '@/components/ui/MultiSelect.vue'
import { useAuthStore } from '@/stores/auth'
import { useClientSort } from '@/composables/useClientSort'
import { useClientPagination } from '@/composables/useClientPagination'

const router = useRouter()
const auth = useAuthStore()

const pipelines = ref<Pipeline[]>([])
const loading = ref(true)
const availableSecrets = ref<{ id: string; name: string; owner_group: string; kind?: string }[]>([])
const availableVariables = ref<{ id: string; name: string; value: string }[]>([])
const availableTeams = ref<Team[]>([])

/** Resolve team_id for create/update: picker value, or sole membership. */
function ensureTeamId(): string {
  if (auth.showTeamPicker) return form.value.team_id.trim()
  if (form.value.team_id.trim()) return form.value.team_id.trim()
  return auth.soleTeamId
}

const searchQuery = ref('')
const filterTrigger = ref('')
const filterSource = ref('')
const filterActive = ref('')

const filteredPipelines = computed(() => {
  let source = pipelines.value.filter((p) => auth.matchesActiveTeam(p.team_id))
  if (filterTrigger.value) source = source.filter((p) => p.trigger_type === filterTrigger.value)
  if (filterSource.value) source = source.filter((p) => p.source === filterSource.value)
  if (filterActive.value) source = source.filter((p) => (filterActive.value === 'active' ? p.is_active : !p.is_active))
  const q = searchQuery.value.trim().toLowerCase()
  if (!q) return source
  return source.filter((p) => {
    return [
      p.name,
      p.description,
      p.trigger_type,
      ...(p.runner_labels || []),
      p.latest_job_status,
      p.concurrency_group,
      p.owner_group,
    ]
      .filter(Boolean)
      .join(' ')
      .toLowerCase()
      .includes(q)
  })
})

const { sorted, toggleSort, sortIndicator } = useClientSort(filteredPipelines, {
  initialKey: 'last_build',
  initialDir: 'desc',
  getValue: (row, key) => {
    switch (key) {
      case 'name': return row.name
      case 'trigger_type': return row.trigger_type
      case 'last_build': return row.latest_job_start || ''
      case 'success_rate': return row.success_rate ?? -1
      case 'job_total': return row.job_total ?? 0
      case 'is_active': return row.is_active ? 1 : 0
      default: return ''
    }
  },
})

const { page, pageSize, pageCount, total, pageItems: pagedPipelines, rangeLabel } = useClientPagination(sorted)

/** Format build success rate: successful jobs ÷ total jobs. */
function formatSuccessRate(p: Pipeline): string {
  if (p.job_total == null || p.job_total === 0 || p.success_rate == null) return '—'
  return `${Math.round(p.success_rate * 100)}%`
}

function successRateTitle(p: Pipeline): string {
  if (p.job_total == null || p.job_total === 0) return 'No builds yet'
  return `${p.job_success ?? 0} successful / ${p.job_total} total builds`
}

// Create/Edit dialog
const showDialog = ref(false)
const dialogMode = ref<'form' | 'yaml'>('form')
const dialogTitle = ref('New Pipeline')
const editingId = ref<string | null>(null)
const form = ref({
  name: '',
  description: '',
  trigger_type: 'manual',
  webhook_path: '',
  cron_expr: '',
  runner_labels: 'linux-x64',
  runner_timeout: '',
  concurrency_group: '',
  concurrency_mode: '',
  concurrency_max_queue: 3,
  concurrency_queue_timeout_seconds: 1800,
  owner_group: '',
  team_id: '',
  var_ids: [] as string[],
  secret_ids: [] as string[],
  env: [] as { name: string; value: string }[],
  steps: [] as StepDef[],
  input_defs: [] as PipelineInputDef[],
})
const saving = ref(false)

const triggeringId = ref<string | null>(null)
const expandedSteps = reactive(new Set<number>())

// Auto-generate webhook path as a random short string (immutable once created)
function genRandomPath(): string {
  const chars = 'abcdefghijklmnopqrstuvwxyz0123456789'
  let result = ''
  for (let i = 0; i < 12; i++) {
    result += chars[Math.floor(Math.random() * chars.length)]
  }
  return result
}

const copiedPath = ref(false)
function copyWebhookUrl(path: string) {
  if (!path) return
  const fullUrl = `${window.location.origin}/api/v1/hooks/${path}`
  navigator.clipboard.writeText(fullUrl)
  copiedPath.value = true
  setTimeout(() => { copiedPath.value = false }, 2000)
}

// Trigger dialog
const triggerTarget = ref<Pipeline | null>(null)
const triggerInputPairs = ref<{ key: string; value: string; required?: boolean; masking?: boolean }[]>([])

// YAML editor
const yamlContent = ref('')
const yamlResult = ref<YamlValidationResult | null>(null)
const yamlValidating = ref(false)
const yamlImportWarnings = ref<string[]>([])
const yamlImportSuccess = ref(false)
const yamlLineCount = computed(() => yamlContent.value.split('\n').length)
const yamlLineNumbers = computed(() => {
  const lines = yamlLineCount.value
  return Array.from({ length: lines }, (_, i) => i + 1).join('\n')
})

// ── Jobs stats panel ──
const allJobs = ref<Job[]>([])
const statsExpanded = ref(false)
const statNow = ref(Date.now()) // used to refresh elapsed timers
let statTimer: ReturnType<typeof setInterval> | null = null

const teamFilteredJobs = computed(() =>
  allJobs.value.filter(j => auth.matchesActiveTeam(j.team_id))
)

const runningJobs = computed(() =>
  teamFilteredJobs.value.filter(j => j.status === 'running')
)
const pendingJobs = computed(() =>
  teamFilteredJobs.value.filter(j => j.status === 'pending')
)
const queuedJobs = computed(() =>
  teamFilteredJobs.value.filter(j => j.status === 'queued')
)
const totalJobCount = computed(() => teamFilteredJobs.value.length)
const hasActiveJobs = computed(() =>
  runningJobs.value.length > 0 || pendingJobs.value.length > 0 || queuedJobs.value.length > 0
)
const activeCount = computed(() =>
  runningJobs.value.length + pendingJobs.value.length + queuedJobs.value.length
)

function elapsed(ts: string | null | undefined): string {
  if (!ts) return '—'
  const diff = Math.max(0, Math.floor((statNow.value - parseUtc(ts)) / 1000))
  if (diff < 60) return `${diff}s`
  if (diff < 3600) return `${Math.floor(diff / 60)}m ${diff % 60}s`
  return `${Math.floor(diff / 3600)}h ${Math.floor((diff % 3600) / 60)}m`
}

function jogNameLabel(j: Job): string {
  return j.pipeline_name || j.pipeline_id?.slice(0, 8) || 'Unknown'
}

function runNum(job: Job): number {
  // Per-pipeline numbering: oldest build = #1, newest = highest
  const same = allJobs.value.filter(j => j.pipeline_id === job.pipeline_id)
  const idx = same.findIndex(j => j.id === job.id)
  return idx >= 0 ? same.length - idx : 0
}

function goJob(jobId: string) {
  router.push(`/jobs/${jobId}`)
}

async function cancelStatsJob(jobId: string) {
  try {
    await jobsApi.cancel(jobId)
    // remove optimistically
    allJobs.value = allJobs.value.filter(j => j.id !== jobId)
  } catch (e) { console.error(e) }
}

async function fetchJobs() {
  try {
    allJobs.value = await jobsApi.list()
  } catch { /* ignore */ }
}

function startStatsTimer() {
  if (statTimer) return
  statTimer = setInterval(() => {
    statNow.value = Date.now()
    if (!statsExpanded.value && hasActiveJobs.value) {
      // keep refreshing even when collapsed for the summary dots
    }
  }, 5000)
}

function stopStatsTimer() {
  if (statTimer) { clearInterval(statTimer); statTimer = null }
}

function toggleStats() {
  statsExpanded.value = !statsExpanded.value
  if (statsExpanded.value) {
    // refresh elapsed every second while expanded
    stopStatsTimer()
    statTimer = setInterval(() => { statNow.value = Date.now() }, 1000)
  } else {
    stopStatsTimer()
    startStatsTimer()
  }
}

onMounted(async () => {
  await loadPipelines()
  fetchJobs()
  startStatsTimer()
  await loadInfrastructureOptions()
})

onUnmounted(() => {
  stopStatsTimer()
})

/** Refresh Secrets / Variables / Teams used by form + YAML import (not cached across Infra edits). */
async function loadInfrastructureOptions() {
  const [secrets, variables, teams] = await Promise.allSettled([
    secretsApi.list(),
    variablesApi.list(),
    teamsApi.list(),
  ])
  if (secrets.status === 'fulfilled') availableSecrets.value = secrets.value
  if (variables.status === 'fulfilled') availableVariables.value = variables.value
  if (teams.status === 'fulfilled') availableTeams.value = teams.value
}

const variableOptions = computed(() =>
  availableVariables.value.map(v => ({ id: v.id, label: v.name }))
)
const secretOptions = computed(() =>
  availableSecrets.value.map(s => ({
    id: s.id,
    label: s.owner_group ? `${s.name} · ${s.owner_group}` : s.name,
  }))
)

/** Step-level secrets (env injection) — all kinds. */
const stepSecretOptions = computed(() =>
  availableSecrets.value.map(s => ({
    id: s.name,
    label: s.owner_group ? `${s.name} · ${s.owner_group}` : s.name,
  }))
)

/** Registry Secret dropdown — only kind=registry credentials. */
const registrySecretOptions = computed(() =>
  availableSecrets.value
    .filter((s) => s.kind === 'registry')
    .map(s => ({
      id: s.name,
      label: s.owner_group ? `${s.name} · ${s.owner_group}` : s.name,
    }))
)

async function loadPipelines() {
  loading.value = true
  try {
    pipelines.value = await pipelinesApi.list()
  } catch (e) {
    console.error(e)
  } finally {
    loading.value = false
  }
}

function openCreate() {
  editingId.value = null
  dialogMode.value = 'form'
  dialogTitle.value = 'New Pipeline'
  form.value = { name: '', description: '', trigger_type: 'manual', webhook_path: genRandomPath(), cron_expr: '', runner_labels: 'linux-x64', runner_timeout: '', concurrency_group: '', concurrency_mode: '', concurrency_max_queue: 3, concurrency_queue_timeout_seconds: 1800, owner_group: '', team_id: auth.preferredTeamId, var_ids: [], secret_ids: [], env: [], steps: [], input_defs: [] }
  yamlContent.value = ''
  yamlResult.value = null
  yamlImportWarnings.value = []
  yamlImportSuccess.value = false
  expandedSteps.clear()
  showDialog.value = true
  void loadInfrastructureOptions()
}

async function openEdit(p: Pipeline) {
  if (p.source === 'yaml') {
    alert('This pipeline is managed by a YAML file. Edit the source file directly — changes will be picked up automatically on the next scan.')
    return
  }
  editingId.value = p.id
  dialogMode.value = 'form'
  dialogTitle.value = 'Edit Pipeline'
  yamlContent.value = ''
  yamlResult.value = null
  showDialog.value = true
  void loadInfrastructureOptions()

  try {
    // Load full pipeline with steps
    const detail = await pipelinesApi.get(p.id)
    form.value = {
      name: detail.name || '',
      description: detail.description || '',
      trigger_type: detail.trigger_type || 'manual',
      webhook_path: detail.webhook_path || '',
      cron_expr: detail.cron_expr || '',
      runner_labels: (detail.runner_labels || []).join(', '),
      runner_timeout: detail.timeout_seconds ? `${detail.timeout_seconds}s` : '',
      concurrency_group: detail.concurrency_group || '',
      concurrency_mode: detail.concurrency_mode || '',
      concurrency_max_queue: detail.concurrency_max_queue ?? 3,
      concurrency_queue_timeout_seconds: detail.concurrency_queue_timeout_seconds ?? 1800,
      owner_group: detail.owner_group || '',
      team_id: detail.team_id || '',
      var_ids: detail.var_ids || [],
      secret_ids: detail.secret_ids || [],
      env: [],
      steps: (detail.tasks || []).map((t: any) => ({
        name: t.name || '',
        run: t.command_template || '',
        uses: t.exec_type || 'shell',
        image: t.image || undefined,
        volumes: parseVolumesToRows(t.volumes) as any,
        secrets: t.step_secrets ? t.step_secrets.split(',').map((s: string) => s.trim()).filter(Boolean) : [],
        host: (t.host_names || t.host_id || '').toString().trim() || undefined,
        host_labels: t.host_labels || undefined,
        dir: t.work_dir || undefined,
        timeout: t.timeout_seconds ? `${t.timeout_seconds}s` : undefined,
        retry: t.retry_count || undefined,
        continue_on_error: t.fail_strategy === 'continue' ? true : undefined,
        privileged: (t as any).privileged === true ? true : undefined,
        registry_secret: (t.registry_secret || '').trim() || undefined,
      })),
      input_defs: normalizeInputDefsForForm(detail.input_defs),
    }
    // Expand all steps
    for (let i = 0; i < form.value.steps.length; i++) expandedSteps.add(i)
  } catch (e) {
    console.error('Failed to load pipeline detail', e)
  }
}

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

function addStep() {
  const idx = form.value.steps.length
  form.value.steps.push({ name: '', run: '', uses: 'shell', volumes: [] as any, secrets: [] })
  expandedSteps.add(idx)
}

function removeStep(idx: number) {
  form.value.steps.splice(idx, 1)
  expandedSteps.delete(idx)
  // re-key expanded indices after removal
  const next = new Set<number>()
  for (const e of expandedSteps) {
    if (e > idx) next.add(e - 1)
    else if (e < idx) next.add(e)
  }
  expandedSteps.clear()
  for (const n of next) expandedSteps.add(n)
}

function toggleStep(idx: number) {
  if (expandedSteps.has(idx)) expandedSteps.delete(idx)
  else expandedSteps.add(idx)
}

/** Check whether the pipeline-level form has any advanced field filled (for auto-expand). */
const pipelineFormHasAdvanced = computed(() => {
  return !!(form.value.runner_timeout.trim() || form.value.concurrency_group.trim() ||
    form.value.concurrency_mode ||
    form.value.var_ids.length > 0 || form.value.secret_ids.length > 0 ||
    form.value.env.length > 0 || form.value.input_defs.length > 0)
})

const yamlEditorRef = ref<HTMLTextAreaElement | null>(null)
function syncYamlScroll() {
  const lines = document.getElementById('yaml-linenums')
  const editor = yamlEditorRef.value
  if (lines && editor) {
    lines.scrollTop = editor.scrollTop
  }
}

function addInputDef() {
  form.value.input_defs.push(emptyInputDef())
}

function removeInputDef(idx: number) {
  form.value.input_defs.splice(idx, 1)
}

function addEnv() {
  form.value.env.push({ name: '', value: '' })
}

function removeEnv(idx: number) {
  form.value.env.splice(idx, 1)
}

async function handleValidate() {
  if (!yamlContent.value.trim()) return
  yamlImportWarnings.value = []
  yamlImportSuccess.value = false
  yamlValidating.value = true
  yamlResult.value = null
  try {
    yamlResult.value = await pipelinesApi.validateYaml(yamlContent.value)
  } catch (e) {
    console.error(e)
    yamlResult.value = { valid: false, name: null, errors: [{ field: 'network', message: 'Validation request failed' }], warnings: [] }
  } finally {
    yamlValidating.value = false
  }
}

async function importYamlToForm() {
  try {
    // Re-fetch Infra lists so secrets/variables created after page load resolve correctly
    await loadInfrastructureOptions()

    const parsed: YamlParsedData | null | undefined = yamlResult.value?.parsed
    if (!parsed) {
      alert('No parsed data available. Validate the YAML first.')
      return
    }

    // ── Collect warnings for unmappable items ──
    const warnings: string[] = []

    // Pipeline-level variables: look up by name → resolve to IDs
    let resolvedVarIds: string[] = []
    if (parsed.variables && parsed.variables.length > 0) {
      const { ids, missing } = resolveNamesToIds(parsed.variables, availableVariables.value)
      resolvedVarIds = ids
      for (const name of missing) {
        warnings.push(`Variable "${name}" not found in Manager. Create it in Infrastructure → Variables first.`)
      }
    }

    // Pipeline-level secrets: look up by name → resolve to IDs
    let resolvedSecretIds: string[] = []
    if (parsed.secrets && parsed.secrets.length > 0) {
      const { ids, missing } = resolveNamesToIds(parsed.secrets, availableSecrets.value)
      resolvedSecretIds = ids
      for (const name of missing) {
        warnings.push(`Secret "${name}" not found in Manager. Create it in Infrastructure → Secrets first.`)
      }
    }

    // Normalize host: if coming from YAML array joined with ",", keep as comma-separated for editing
    const normalizeHost = (h: string | null | undefined) => {
      if (!h) return undefined
      return h.split(',').map(el => el.trim()).filter(Boolean).join(', ') || undefined
    }

    form.value = {
      name: yamlResult.value?.name || '',
      description: parsed.description || '',
      trigger_type: parsed.trigger_type || 'manual',
      webhook_path: parsed.trigger_config?.webhook_path || '',
      cron_expr: parsed.trigger_config?.cron_expr || '',
      runner_labels: (parsed.runner?.labels || []).join(', ') || 'linux-x64',
      runner_timeout: parsed.runner?.timeout || '',
      concurrency_group: parsed.concurrency || '',
      concurrency_mode: parsed.concurrency_config?.mode ?? '',
      concurrency_max_queue: parsed.concurrency_config?.max_queue ?? 3,
      concurrency_queue_timeout_seconds: 1800,
      owner_group: '',
      team_id: auth.preferredTeamId,
      var_ids: resolvedVarIds,
      secret_ids: resolvedSecretIds,
      env: parsed.env
        ? Object.entries(parsed.env).map(([name, value]) => ({ name, value: String(value) }))
        : [],
      steps: parsed.steps.map(s => ({
        name: s.name || '',
        run: s.run || '',
        uses: s.uses || 'shell',
        image: s.image || undefined,
        volumes: s.volumes
          ? parseVolumesToRows(s.volumes) as any
          : [] as any,
        secrets: s.secrets || [],
        host: normalizeHost(s.host),
        host_labels: normalizeHost(s.host_labels),
        dir: s.dir || undefined,
        timeout: s.timeout || undefined,
        retry: s.retry || undefined,
        continue_on_error: s.continue_on_error || undefined,
        privileged: s.privileged || undefined,
        registry_secret: s.registry_secret || undefined,
      })),
      input_defs: normalizeInputDefsForForm(
        (parsed.trigger_config?.inputs || []).map((inp) => ({
          name: inp.name || '',
          description: inp.description || '',
          required: !!inp.required,
          default: inp.default || '',
          masking: !!inp.masking,
        })),
      ),
    }

    // Expand all steps
    expandedSteps.clear()
    for (let i = 0; i < form.value.steps.length; i++) expandedSteps.add(i)

    // Stay on YAML tab — let the user review results before switching to Form
    yamlImportWarnings.value = warnings
    yamlImportSuccess.value = warnings.length === 0
  } catch (e) {
    console.error('Failed to import YAML to form', e)
  }
}

async function handleSave() {
  if (!form.value.name.trim()) return
  if (form.value.steps.length === 0) { alert('Add at least one step'); return }
  const teamId = ensureTeamId()
  if (!teamId) { alert('Team is required'); return }
  form.value.team_id = teamId
  for (const s of form.value.steps) {
    if (s.uses === 'docker' && !(s.image || '').trim()) {
      alert(`Step "${s.name || 'unnamed'}": Docker engine requires Image`)
      return
    }
    if (s.uses === 'ssh') {
      const hasHost = !!(s.host || '').toString().trim()
      const hasLabels = !!(s.host_labels || '').toString().trim()
      if (!hasHost && !hasLabels) {
        alert(`Step "${s.name || 'unnamed'}": SSH requires SSH Host and/or Host Labels`)
        return
      }
    }
  }

  saving.value = true
  try {
    const labels = form.value.runner_labels.split(',').map(s => s.trim()).filter(Boolean)
    let timeout_seconds: number | undefined = undefined
    if (form.value.runner_timeout.trim()) {
      const raw = form.value.runner_timeout.trim()
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
      name: form.value.name.trim(),
      description: form.value.description.trim() || undefined,
      trigger_type: form.value.trigger_type,
      webhook_path: form.value.trigger_type === 'webhook' ? (form.value.webhook_path || genRandomPath()) : undefined,
      cron_expr: form.value.trigger_type === 'cron' ? form.value.cron_expr || undefined : undefined,
      runner_labels: labels,
      timeout_seconds,
      concurrency_group: form.value.concurrency_group.trim() || undefined,
      concurrency_mode: form.value.concurrency_group.trim() ? (form.value.concurrency_mode || undefined) : undefined,
      concurrency_max_queue: form.value.concurrency_mode === 'queue' ? form.value.concurrency_max_queue : undefined,
      concurrency_queue_timeout_seconds: form.value.concurrency_mode === 'queue' ? form.value.concurrency_queue_timeout_seconds : undefined,
      team_id: form.value.team_id,
      var_ids: form.value.var_ids.length > 0 ? form.value.var_ids : undefined,
      secret_ids: form.value.secret_ids.length > 0 ? form.value.secret_ids : undefined,
      env: form.value.env.filter(e => e.name.trim() && e.value.trim()).length > 0
        ? form.value.env.filter(e => e.name.trim() && e.value.trim()).map(e => ({ name: e.name.trim(), value: e.value }))
        : undefined,
      steps: form.value.steps.map(s => {
        const rawHost = s.host as string | undefined
        const rawHostLabels = s.host_labels as string | undefined
        const isDocker = s.uses === 'docker'
        const volumesArray = isDocker ? rowsToVolumesArray((s as any).volumes || []) : undefined
        return {
          ...s,
          image: isDocker ? (s.image || '').trim() || undefined : undefined,
          volumes: volumesArray,
          privileged: isDocker ? (s as any).privileged === true || undefined : undefined,
          registry_secret: ((s as any).registry_secret || '').trim() || undefined,
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
      input_defs: form.value.trigger_type === 'manual'
        ? normalizeInputDefsForSave(form.value.input_defs)
        : undefined,
    }

    if (editingId.value) {
      await pipelinesApi.update(editingId.value, payload)
    } else {
      await pipelinesApi.create(payload)
    }

    showDialog.value = false
    editingId.value = null
    await loadPipelines()
  } catch (e) {
    console.error(e)
    alert(editingId.value ? 'Failed to update pipeline' : 'Failed to create pipeline')
  } finally {
    saving.value = false
  }
}

async function handleTrigger(p: Pipeline) {
  if (triggeringId.value) return

  // Prefer fresh defs (incl. masking) from detail; fall back to list payload.
  let defs: PipelineInputDef[] = Array.isArray(p.input_defs) ? p.input_defs : []
  try {
    const detail = await pipelinesApi.get(p.id)
    if (Array.isArray(detail.input_defs)) defs = detail.input_defs
  } catch {
    /* use list payload */
  }

  if (defs.length > 0) {
    triggerTarget.value = p
    triggerInputPairs.value = normalizeInputDefsForForm(defs).map((d) => ({
      key: d.name,
      value: d.default || '',
      required: d.required,
      masking: !!d.masking,
    }))
    return
  }

  // No input_defs — trigger directly
  await doTrigger(p)
}

async function doTrigger(p: Pipeline) {
  if (triggeringId.value) return
  triggeringId.value = p.id
  try {
    const inputs: Record<string, string> = {}
    for (const pair of triggerInputPairs.value) {
      if (pair.key.trim()) inputs[pair.key.trim()] = pair.value
    }
    const result = await pipelinesApi.trigger(p.id, inputs)
    triggerTarget.value = null
    triggerInputPairs.value = []
    router.push(`/jobs/${result.job_id}`)
  } catch (e) {
    console.error(e)
  } finally {
    triggeringId.value = null
  }
}

function cancelTriggerDialog() {
  triggerTarget.value = null
  triggerInputPairs.value = []
  triggeringId.value = null
}

async function handleDelete(p: Pipeline) {
  if (!confirm(`Delete pipeline "${p.name}"?`)) return
  try {
    await pipelinesApi.delete(p.id)
    await loadPipelines()
  } catch (e) {
    console.error(e)
    alert('Delete failed')
  }
}

async function handleToggleActive(p: Pipeline) {
  try {
    const result = await pipelinesApi.toggleActive(p.id)
    p.is_active = result.is_active
  } catch (e) {
    console.error(e)
  }
}
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      title="Pipelines"
      description="Create and manage your pipeline workflows."
    >
      <template #actions>
        <UiButton size="sm" @click="openCreate">
          <Plus class="h-4 w-4" />
          New Pipeline
        </UiButton>
      </template>
    </PageHeader>

    <!-- ── Jobs stats panel ── -->
    <div v-if="!loading && hasActiveJobs">
      <div
        class="flex items-center px-1 py-3.5 cursor-pointer select-none hover:bg-muted/10 transition-colors"
        @click="toggleStats"
      >
        <!-- Left: state dots -->
        <div class="flex items-center gap-5">
          <span v-if="runningJobs.length > 0" class="inline-flex items-baseline gap-2">
            <span class="w-2.5 h-2.5 rounded-full bg-amber-500" />
            <span class="text-sm font-semibold tabular-nums text-amber-500">{{ runningJobs.length }}</span>
            <span class="text-xs text-amber-500/70 leading-none">running</span>
          </span>
          <span v-if="pendingJobs.length > 0" class="inline-flex items-baseline gap-2">
            <span class="w-2.5 h-2.5 rounded-full bg-slate-400" />
            <span class="text-sm font-semibold tabular-nums text-muted-foreground">{{ pendingJobs.length }}</span>
            <span class="text-xs text-muted-foreground/50 leading-none">pending</span>
          </span>
          <span v-if="queuedJobs.length > 0" class="inline-flex items-baseline gap-2">
            <span class="w-2.5 h-2.5 rounded-full bg-purple-400" />
            <span class="text-sm font-semibold tabular-nums text-purple-400">{{ queuedJobs.length }}</span>
            <span class="text-xs text-purple-400/70 leading-none">queued</span>
          </span>
        </div>

        <!-- Right: summary -->
        <div class="ml-auto flex items-center gap-2">
          <span class="inline-flex items-baseline gap-1">
            <span class="text-sm font-semibold tabular-nums text-muted-foreground/80">{{ activeCount }}</span>
            <span class="text-xs text-muted-foreground/40 leading-none">of</span>
            <span class="text-sm tabular-nums text-muted-foreground/60">{{ totalJobCount }}</span>
          </span>
          <button
            class="w-5 h-5 flex items-center justify-center"
          >
            <ChevronDown v-if="statsExpanded" class="h-4 w-4 text-muted-foreground/40" />
            <ChevronRight v-else class="h-4 w-4 text-muted-foreground/40" />
          </button>
        </div>
      </div>

      <!-- Expanded detail panels -->
      <template v-if="statsExpanded">
        <!-- Running -->
        <template v-if="runningJobs.length > 0">
          <div class="border-t border-border/20" />
          <div class="px-5 py-2 text-xs uppercase tracking-wider text-muted-foreground font-semibold flex items-center justify-between">
            <span>Running</span>
            <span class="font-mono tabular-nums text-xs">{{ runningJobs.length }} job{{ runningJobs.length !== 1 ? 's' : '' }}</span>
          </div>
          <div class="divide-y divide-border/10">
            <div
              v-for="j in runningJobs"
              :key="j.id"
              class="px-5 py-2 flex items-center gap-3 text-sm hover:bg-muted/30 transition-colors cursor-pointer"
              @click="goJob(j.id)"
            >
              <span class="font-mono text-xs w-10 tabular-nums text-muted-foreground/60 shrink-0">#{{ runNum(j) }}</span>
              <span class="flex-1 font-medium text-foreground/80 truncate">{{ jogNameLabel(j) }}</span>
              <span class="text-xs font-mono tabular-nums text-amber-500/80 shrink-0">
                <Clock class="h-3.5 w-3.5 inline mr-0.5 -mt-px" />{{ elapsed(j.start_time) }}
              </span>
              <button
                class="shrink-0 ml-1 rounded p-0.5 hover:bg-red-500/10 transition-colors"
                title="Cancel"
                @click.stop="cancelStatsJob(j.id)"
              >
                <X class="h-4 w-4 text-red-400/70 hover:text-red-500 transition-colors" />
              </button>
            </div>
          </div>
        </template>

        <!-- Pending -->
        <template v-if="pendingJobs.length > 0">
          <div class="border-t border-border/20" />
          <div class="px-5 py-2 text-xs uppercase tracking-wider text-muted-foreground font-semibold flex items-center justify-between">
            <span>Pending</span>
            <span class="font-mono tabular-nums text-xs">{{ pendingJobs.length }} job{{ pendingJobs.length !== 1 ? 's' : '' }}</span>
          </div>
          <div class="divide-y divide-border/10">
            <div
              v-for="j in pendingJobs"
              :key="j.id"
              class="px-5 py-2 flex items-center gap-3 text-sm hover:bg-muted/30 transition-colors cursor-pointer"
              @click="goJob(j.id)"
            >
              <span class="font-mono text-xs w-10 tabular-nums text-muted-foreground/60 shrink-0">#{{ runNum(j) }}</span>
              <span class="flex-1 font-medium text-foreground/80 truncate">{{ jogNameLabel(j) }}</span>
              <span class="text-xs font-mono tabular-nums text-muted-foreground/50 shrink-0">{{ elapsed(j.created_at) }} ago</span>
              <button
                class="shrink-0 ml-1 rounded p-0.5 hover:bg-red-500/10 transition-colors"
                title="Cancel"
                @click.stop="cancelStatsJob(j.id)"
              >
                <X class="h-4 w-4 text-red-400/70 hover:text-red-500 transition-colors" />
              </button>
            </div>
          </div>
        </template>

        <!-- Queued -->
        <template v-if="queuedJobs.length > 0">
          <div class="border-t border-border/20" />
          <div class="px-5 py-2 text-xs uppercase tracking-wider text-muted-foreground font-semibold flex items-center justify-between">
            <span>Queued</span>
            <span class="font-mono tabular-nums text-xs">{{ queuedJobs.length }} job{{ queuedJobs.length !== 1 ? 's' : '' }}</span>
          </div>
          <div class="divide-y divide-border/10">
            <div
              v-for="j in queuedJobs"
              :key="j.id"
              class="px-5 py-2 flex items-center gap-3 text-sm hover:bg-muted/30 transition-colors cursor-pointer"
              @click="goJob(j.id)"
            >
              <span class="font-mono text-xs w-10 tabular-nums text-muted-foreground/60 shrink-0">#{{ runNum(j) }}</span>
              <span class="flex-1 font-medium text-foreground/80 truncate">{{ jogNameLabel(j) }}</span>
              <span class="text-xs font-mono tabular-nums text-muted-foreground/50 shrink-0">{{ elapsed(j.created_at) }} ago</span>
              <button
                class="shrink-0 ml-1 rounded p-0.5 hover:bg-red-500/10 transition-colors"
                title="Cancel"
                @click.stop="cancelStatsJob(j.id)"
              >
                <X class="h-4 w-4 text-red-400/70 hover:text-red-500 transition-colors" />
              </button>
            </div>
          </div>
        </template>

        <div class="border-t border-border/20" />
        <div class="px-5 py-2 text-center">
          <button
            class="text-xs text-muted-foreground hover:text-foreground transition-colors"
            @click="fetchJobs"
          >Refresh</button>
        </div>
      </template>
    </div>

    <div v-if="loading" class="surface-card p-8 text-center type-meta">Loading...</div>

    <template v-else>
      <div v-if="filteredPipelines.length === 0" class="surface-card p-8 text-center type-meta">
        {{ pipelines.length === 0 ? 'No pipelines created yet' : 'No pipelines in the selected team' }}
      </div>

      <div v-else class="surface-card overflow-hidden">
        <!-- Filter bar -->
        <div class="px-5 py-3 border-b border-border/30 flex items-center gap-3 flex-wrap">
          <input
            v-model="searchQuery"
            type="text"
            placeholder="Search pipelines…"
            class="w-full max-w-52 rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50 focus:ring-1 focus:ring-primary/10"
          />
          <select v-model="filterTrigger" class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50">
            <option value="">All triggers</option>
            <option value="manual">Manual</option>
            <option value="webhook">Webhook</option>
            <option value="cron">Cron</option>
          </select>
          <select v-model="filterSource" class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50">
            <option value="">All sources</option>
            <option value="form">Form</option>
            <option value="yaml">YAML</option>
          </select>
          <select v-model="filterActive" class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50">
            <option value="">All</option>
            <option value="active">Active</option>
            <option value="inactive">Inactive</option>
          </select>
          <span v-if="filteredPipelines.length !== pipelines.length" class="text-xs text-muted-foreground ml-auto">
            {{ filteredPipelines.length }} of {{ pipelines.length }}
          </span>
        </div>
        <div class="overflow-x-auto">
        <table class="data-table">
          <thead>
            <tr>
              <th>
                <button type="button" class="inline-flex items-center gap-1 hover:text-foreground transition-colors" @click="toggleSort('name')">
                  Name
                  <span class="font-mono text-xs opacity-50">{{ sortIndicator('name') || '↕' }}</span>
                </button>
              </th>
              <th>
                <button type="button" class="inline-flex items-center gap-1 hover:text-foreground transition-colors" @click="toggleSort('trigger_type')">
                  Trigger
                  <span class="font-mono text-xs opacity-50">{{ sortIndicator('trigger_type') || '↕' }}</span>
                </button>
              </th>
              <th>R-Labels</th>
              <th>
                <button type="button" class="inline-flex items-center gap-1 hover:text-foreground transition-colors" @click="toggleSort('last_build')">
                  Last Build
                  <span class="font-mono text-xs opacity-50">{{ sortIndicator('last_build') || '↕' }}</span>
                </button>
              </th>
              <th>
                <button type="button" class="inline-flex items-center gap-1 hover:text-foreground transition-colors" @click="toggleSort('success_rate')" title="Successful jobs ÷ total builds">
                  Rate
                  <span class="font-mono text-xs opacity-50">{{ sortIndicator('success_rate') || '↕' }}</span>
                </button>
              </th>
              <th>
                <button type="button" class="inline-flex items-center gap-1 hover:text-foreground transition-colors" @click="toggleSort('is_active')">
                  Active
                  <span class="font-mono text-xs opacity-50">{{ sortIndicator('is_active') || '↕' }}</span>
                </button>
              </th>
              <th class="text-right">Actions</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="p in pagedPipelines" :key="p.id">
              <td>
                <div class="flex items-center gap-2">
                  <button
                    class="font-medium text-left hover:text-primary hover:underline underline-offset-2 transition-colors cursor-pointer"
                    @click="router.push(`/pipelines/${p.id}/stage`)"
                  >{{ p.name }}</button>
                  <span
                    v-if="Array.isArray(p.input_defs) && p.input_defs.length > 0"
                    class="inline-flex items-center gap-1 rounded-md border border-border/60 bg-muted/40 px-2 py-0.5 text-xs text-muted-foreground"
                    :title="(p.input_defs || []).some((d) => d.masking) ? 'Params (some masking)' : 'Params'"
                  >
                    <Puzzle class="h-3 w-3" />
                    Params
                    <span
                      v-if="(p.input_defs || []).some((d) => d.masking)"
                      class="text-[9px] uppercase tracking-wide text-violet-600/90"
                    >mask</span>
                  </span>
                </div>
                <div v-if="p.description" class="text-xs text-muted-foreground mt-0.5">{{ p.description }}</div>
              </td>
              <td>
                <StatusBadge :status="p.trigger_type" />
              </td>
              <td>
                <span
                  v-for="l in p.runner_labels"
                  :key="l"
                  class="mono-chip mr-1"
                >{{ l }}</span>
              </td>
              <td>
                <button
                  v-if="p.latest_job_id"
                  class="inline-flex flex-col items-start gap-0.5 group cursor-pointer text-left"
                  :title="`Job #${p.latest_job_id.slice(0, 8)} — ${p.latest_job_status}`"
                  @click="router.push(`/jobs/${p.latest_job_id}`)"
                >
                  <span class="inline-flex items-center gap-1.5">
                    <span
                      :class="cn(
                        'w-2 h-2 rounded-full shrink-0',
                        p.latest_job_status === 'success' ? 'bg-emerald-500' :
                        p.latest_job_status === 'failed' ? 'bg-red-500' :
                        p.latest_job_status === 'running' ? 'bg-amber-400 animate-pulse' :
                        'bg-slate-300'
                      )"
                    />
                    <span
                      :class="cn(
                        'text-xs font-medium group-hover:underline',
                        p.latest_job_status === 'success' ? 'text-emerald-600' :
                        p.latest_job_status === 'failed' ? 'text-red-500' :
                        p.latest_job_status === 'running' ? 'text-amber-600' :
                        'text-muted-foreground'
                      )"
                    >{{ p.latest_job_status || '—' }}</span>
                  </span>
                  <span
                    v-if="p.latest_job_start"
                    class="text-xs text-muted-foreground pl-3.5 tabular-nums"
                  >{{ p.latest_job_start.slice(5, 19) }}</span>
                </button>
                <span v-else class="inline-flex items-center gap-1 text-xs text-muted-foreground">
                  <Minus class="h-3 w-3" />
                  No runs
                </span>
              </td>
              <td>
                <span
                  class="text-xs font-mono tabular-nums"
                  :class="p.job_total ? 'text-foreground/80' : 'text-muted-foreground'"
                  :title="successRateTitle(p)"
                >{{ formatSuccessRate(p) }}</span>
                <span
                  v-if="p.job_total"
                  class="block text-xs text-muted-foreground mt-0.5 tabular-nums"
                  :title="successRateTitle(p)"
                >{{ p.job_success }}/{{ p.job_total }}</span>
              </td>
              <td>
                <StatusBadge :status="p.is_active ? 'online' : 'offline'" />
              </td>
              <td class="text-right">
                <div class="flex items-center justify-end gap-1">
                  <UiButton variant="ghost" size="sm" @click="handleTrigger(p)" title="Trigger run" :disabled="triggeringId !== null">
                    <Loader2 v-if="triggeringId === p.id" class="h-3.5 w-3.5 animate-spin" />
                    <Play v-else class="h-3.5 w-3.5" />
                  </UiButton>
                  <UiButton variant="ghost" size="sm" @click="openEdit(p)" :title="p.source === 'yaml' ? 'Edit in YAML file' : 'Edit'" :disabled="p.source === 'yaml'">
                    <Pencil class="h-3.5 w-3.5" :class="{ 'opacity-40': p.source === 'yaml' }" />
                  </UiButton>
                  <UiButton variant="ghost" size="sm" @click="handleToggleActive(p)" :title="p.is_active ? 'Deactivate' : 'Activate'">
                    <Snowflake class="h-3.5 w-3.5" :class="p.is_active ? 'text-cyan-500' : 'text-muted-foreground'" />
                  </UiButton>
                  <UiButton variant="ghost" size="sm" @click="handleDelete(p)" title="Delete">
                    <Trash2 class="h-3.5 w-3.5 text-destructive" />
                  </UiButton>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
        </div>
        <TablePagination v-model:page="page" v-model:pageSize="pageSize" :total="total" :page-count="pageCount" :range-label="rangeLabel" />
      </div>
    </template>

    <!-- Trigger dialog -->
    <Teleport to="body">
      <div v-if="triggerTarget" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/50" @click="cancelTriggerDialog()" />
        <div class="relative z-10 w-full max-w-md rounded-xl border border-border bg-background p-6 shadow-2xl">
          <h2 class="text-base font-semibold mb-1">Trigger Pipeline</h2>
          <p class="text-sm text-muted-foreground mb-4">
            Pass dynamic parameters. Each key is available as <code class="mono-chip">$INPUT_&lt;KEY&gt;</code> in step commands.
            Masked fields use a password input and are redacted in logs / encrypted at rest.
          </p>

          <div class="space-y-2 mb-6">
            <div
              v-for="(pair, idx) in triggerInputPairs"
              :key="idx"
              class="flex items-center gap-2"
            >
              <div
                class="flex-1 rounded-md border border-border/40 bg-muted/30 px-3 py-2 text-sm font-mono text-muted-foreground select-none flex items-center gap-1.5 flex-wrap"
              >
                {{ pair.key }}
                <span v-if="pair.required" class="text-red-500 text-sm font-sans leading-none">*</span>
                <span v-else class="text-xs text-muted-foreground/50 font-sans">(optional)</span>
                <span
                  v-if="pair.masking"
                  class="text-[10px] font-medium uppercase tracking-wide rounded px-1.5 py-0.5 bg-violet-500/10 text-violet-600 border border-violet-500/20 font-sans"
                >masking</span>
              </div>
              <input
                v-model="pair.value"
                :type="pair.masking ? 'password' : 'text'"
                :placeholder="pair.required ? 'required' : 'enter value (leave empty for default)'"
                class="flex-[2] rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
                :disabled="triggeringId !== null"
                :autocomplete="pair.masking ? 'new-password' : 'off'"
              />
            </div>
          </div>

          <div class="flex items-center justify-end gap-3">
            <UiButton variant="outline" size="sm" @click="cancelTriggerDialog()">Cancel</UiButton>
            <UiButton size="sm" @click="doTrigger(triggerTarget!)" :disabled="triggeringId !== null">
              <Loader2 v-if="triggeringId === triggerTarget?.id" class="mr-1.5 h-4 w-4 animate-spin" />
              <Play v-else class="mr-1.5 h-4 w-4" /> Run
            </UiButton>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- Create dialog -->
    <Teleport to="body">
      <div
        v-if="showDialog"
        class="fixed inset-0 z-50 flex items-start justify-center pt-[8vh]"
        @click.self="showDialog = false"
      >
        <div class="soft-panel w-full max-w-xl p-6 space-y-5 max-h-[85vh] overflow-y-auto">
          <div class="flex items-center justify-between">
            <h2 class="text-base font-semibold tracking-tight">{{ dialogTitle }}</h2>
            <button
              type="button"
              class="inline-flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground"
              @click="showDialog = false"
            >
              <X class="h-4 w-4" />
            </button>
          </div>

          <!-- Tab switcher -->
          <div class="flex rounded-xl bg-muted p-1">
            <button
              class="flex-1 flex items-center justify-center gap-1.5 rounded-lg px-3 py-2 text-sm font-medium transition-colors"
              :class="dialogMode === 'form' ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'"
              @click="dialogMode = 'form'"
            >
              <FileText class="h-3.5 w-3.5" />
              Form
            </button>
            <button
              class="flex-1 flex items-center justify-center gap-1.5 rounded-lg px-3 py-2 text-sm font-medium transition-colors"
              :class="dialogMode === 'yaml' ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'"
              @click="dialogMode = 'yaml'"
            >
              <Code class="h-3.5 w-3.5" />
              YAML
            </button>
          </div>

          <!-- Form tab -->
          <template v-if="dialogMode === 'form'">
            <div class="space-y-4">
              <div>
                <label class="type-label block mb-1.5">Name <span class="text-red-500">*</span></label>
                <input
                  v-model="form.name"
                  type="text"
                  placeholder="e.g. hello-world"
                  class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                />
              </div>
              <div>
                <label class="type-label block mb-1.5">Description</label>
                <input
                  v-model="form.description"
                  type="text"
                  placeholder="Optional description"
                  class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                />
              </div>
              <div class="grid grid-cols-2 gap-4">
                <div>
                  <label class="type-label block mb-1.5">Trigger</label>
                  <select
                    v-model="form.trigger_type"
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
                    v-model="form.runner_labels"
                    type="text"
                    placeholder="linux-x64, docker"
                    class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                  />
                  <p class="text-xs text-muted-foreground mt-1">
                    Optional. Comma-separated; runner must have <span class="font-medium text-foreground">all</span> listed labels (AND) to claim jobs.
                  </p>
                </div>
              </div>

              <div v-if="form.trigger_type === 'webhook'" class="rounded-xl border border-border/70 bg-muted/20 p-4 space-y-3">
                <div class="flex items-center gap-2">
                  <Code class="h-4 w-4 text-muted-foreground" />
                  <span class="text-sm font-medium">Webhook Configuration</span>
                </div>
                <div>
                  <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">URL Path</label>
                  <div class="flex items-center gap-2">
                    <div class="flex items-center gap-1 rounded-lg border border-border bg-muted/50 px-3 py-1.5 text-sm font-mono flex-1 min-w-0">
                      <span class="text-muted-foreground shrink-0">/api/v1/hooks/</span>
                      <span class="text-foreground truncate">{{ form.webhook_path || '—' }}</span>
                    </div>
                    <button
                      v-if="form.webhook_path"
                      type="button"
                      class="shrink-0 inline-flex h-7 w-7 items-center justify-center rounded-md text-muted-foreground hover:bg-muted hover:text-foreground transition-colors"
                      title="Copy full URL"
                      @click="copyWebhookUrl(form.webhook_path)"
                    >
                      <Check v-if="copiedPath" class="h-3.5 w-3.5 text-emerald-500" />
                      <Copy v-else class="h-3.5 w-3.5" />
                    </button>
                  </div>
                  <p class="text-xs text-muted-foreground mt-1.5">
                    Send POST requests to this URL to trigger execution.
                  </p>
                </div>
              </div>

              <div v-if="form.trigger_type === 'cron'" class="rounded-xl border border-border/70 bg-muted/20 p-4 space-y-3">
                <div class="flex items-center gap-2">
                  <Code class="h-4 w-4 text-muted-foreground" />
                  <span class="text-sm font-medium">Cron Configuration</span>
                </div>
                <div>
                  <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Schedule (cron expression)</label>
                  <input
                    v-model="form.cron_expr"
                    type="text"
                    placeholder="0 2 * * *"
                    class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-sm font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                  />
                  <p class="text-xs text-muted-foreground mt-1.5">
                    Standard 5‑field cron syntax: minute hour day month weekday.
                  </p>
                  <details class="mt-2">
                    <summary class="text-xs text-muted-foreground cursor-pointer hover:text-foreground">Quick reference</summary>
                    <div class="mt-1.5 grid grid-cols-2 gap-x-4 gap-y-0.5 text-xs font-mono text-muted-foreground">
                      <span>*/5 * * * *</span><span>Every 5 min</span>
                      <span>0 * * * *</span><span>Hourly</span>
                      <span>0 2 * * *</span><span>Daily 2 AM</span>
                      <span>0 0 * * 0</span><span>Weekly Sun</span>
                    </div>
                  </details>
                </div>
              </div>

              <div v-if="auth.showTeamPicker">
                <label class="type-label block mb-1.5">Team <span class="text-red-500">*</span></label>
                <select
                  v-model="form.team_id"
                  class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                >
                  <option value="" disabled>Select a team…</option>
                  <option v-for="t in availableTeams" :key="t.id" :value="t.id">
                    {{ t.name }} ({{ t.slug }})
                  </option>
                </select>
                <p class="mt-1 text-xs text-muted-foreground">
                  Only secrets with <span class="font-medium text-foreground">Team share</span> enabled are auto-injected.
                  Otherwise bind via Authorized Secrets or YAML / step / registry_secret references (same team only).
                </p>
              </div>

              <!-- Steps editor -->
              <div>
                <div class="flex items-center justify-between mb-1.5">
                  <span class="type-label">Steps <span class="text-red-500">*</span></span>
                  <span class="text-xs text-muted-foreground font-mono">{{ form.steps.length }} step(s)</span>
                </div>

                <button
                  v-if="form.steps.length === 0"
                  type="button"
                  class="w-full inline-flex items-center justify-center gap-2 rounded-xl border border-dashed border-border px-3 py-2.5 text-sm text-muted-foreground hover:border-foreground/25 hover:bg-muted/20 hover:text-foreground transition-colors"
                  @click="addStep"
                >
                  <span class="inline-flex items-center gap-1.5 font-medium">
                    <Plus class="h-3.5 w-3.5" />
                    Add Step
                  </span>
                  <span class="text-border">·</span>
                  <span class="text-xs font-normal text-muted-foreground/70">No steps defined yet</span>
                </button>

                <template v-else>
                <div class="space-y-2 mb-3">
                  <div
                    v-for="(step, idx) in form.steps"
                    :key="idx"
                    class="rounded-xl border border-border/70 overflow-hidden transition-colors"
                    :class="expandedSteps.has(idx) ? 'bg-card' : 'bg-muted/30'"
                  >
                    <!-- Step header (always visible) -->
                    <button
                      type="button"
                      class="w-full flex items-center gap-2.5 px-3 py-2.5 text-left hover:bg-muted/30 transition-colors"
                      @click="toggleStep(idx)"
                    >
                      <span class="inline-flex items-center justify-center h-6 w-6 rounded-md bg-muted font-mono text-xs text-muted-foreground shrink-0">
                        {{ idx + 1 }}
                      </span>
                      <div class="flex-1 min-w-0">
                        <div v-if="step.name" class="text-sm font-medium truncate">{{ step.name }}</div>
                        <div v-if="step.run" class="text-xs font-mono text-muted-foreground truncate mt-0.5">{{ step.run.slice(0, 60) }}</div>
                        <div v-if="!step.name && !step.run" class="text-xs text-muted-foreground italic">Empty step</div>
                      </div>
                      <div class="flex items-center gap-1 shrink-0">
                        <span v-if="step.uses" class="mono-chip text-xs">{{ step.uses }}</span>
                        <span v-if="step.continue_on_error" class="mono-chip text-xs text-amber-600 bg-amber-500/10">no-fail</span>
                        <button
                          type="button"
                          class="inline-flex h-6 w-6 items-center justify-center rounded text-muted-foreground hover:bg-destructive/10 hover:text-destructive"
                          @click.stop="removeStep(idx)"
                        >
                          <X class="h-3.5 w-3.5" />
                        </button>
                        <ChevronDown
                          v-if="expandedSteps.has(idx)"
                          class="h-4 w-4 text-muted-foreground transition-transform"
                        />
                        <ChevronRight
                          v-else
                          class="h-4 w-4 text-muted-foreground transition-transform"
                        />
                      </div>
                    </button>

                    <!-- Step body (expanded) -->
                    <div v-if="expandedSteps.has(idx)" class="px-4 pb-4 space-y-3 border-t border-border/40 pt-3">
                      <div class="grid grid-cols-5 gap-3">
                        <div class="col-span-3">
                          <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Step Name <span class="text-red-500">*</span></label>
                          <input
                            v-model="step.name"
                            type="text"
                            placeholder="e.g. Install dependencies"
                            class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                          />
                        </div>
                        <div class="col-span-2">
                          <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Engine</label>
                          <select
                            v-model="step.uses"
                            class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                          >
                            <option value="shell">Shell</option>
                            <option value="ssh">SSH</option>
                            <option value="docker">Docker</option>
                          </select>
                        </div>
                      </div>

                      <div v-if="step.uses === 'docker'">
                        <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">
                          Image <span class="text-red-500">*</span>
                        </label>
                        <input
                          v-model="step.image"
                          type="text"
                          placeholder="e.g. alpine:3.20 or crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com/ns/app:1.0"
                          class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-sm font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                        />
                      </div>

                      <div>
                        <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">
                          Registry Secret
                        </label>
                        <select
                          v-model="(step as any).registry_secret"
                          class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-sm font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                        >
                          <option value="">None</option>
                          <option v-for="opt in registrySecretOptions" :key="opt.id" :value="opt.id">
                            {{ opt.label }}
                          </option>
                        </select>
                        <p class="text-xs text-muted-foreground/70 mt-0.5">
                          Only Secrets with Kind = Registry. Used for <code class="font-mono">docker login</code> (pull or push).
                          JSON value (encrypted):
                          <code class="font-mono">{ "provider", "registry", "username", "password" }</code>.
                          Shell/SSH: login before step; Docker: login before pull. Not injected as env.
                        </p>
                      </div>

                                            <div v-if="step.uses === 'docker'">
                        <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">
                          Volumes
                        </label>
                        <div v-if="((step as any).volumes || []).length > 0" class="rounded-lg border border-border overflow-hidden mb-1.5">
                          <div class="grid grid-cols-[1fr_1fr_28px] gap-0 text-xs font-medium uppercase tracking-wider text-muted-foreground bg-muted/30 border-b border-border px-3 py-1.5">
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
                          class="flex items-center gap-1 px-3 py-1.5 text-xs font-medium text-muted-foreground hover:text-foreground bg-muted/20 hover:bg-muted/40 rounded-lg border border-dashed border-border/60 transition-colors"
                          @click="
                            if (!(step as any).volumes) (step as any).volumes = [];
                            (step as any).volumes.push({ host: '', container: '' })
                          "
                        >
                          <svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/><path d="M12 5v14"/></svg>
                          Add Volume
                        </button>
                        <p class="text-xs text-muted-foreground/70 mt-1">Map host paths into the container. All host paths must exist on the runner.</p>
                      </div>

                      <div v-if="step.uses === 'docker'" class="mb-3">
                        <label class="flex items-start gap-2 cursor-pointer select-none">
                          <input
                            type="checkbox"
                            v-model="(step as any).privileged"
                            class="mt-0.5 h-4 w-4 rounded border-border accent-red-500"
                          />
                          <div>
                            <span class="text-xs font-medium text-foreground">Privileged mode</span>
                            <span class="ml-1 px-1 py-0.5 text-xs uppercase tracking-wider rounded bg-red-500/10 text-red-600 font-semibold">Security</span>
                            <p class="text-xs text-muted-foreground/70 mt-0.5">Required for Docker-in-Docker (docker:dind). Grants full host capabilities — only use on disposable runner VMs.</p>
                          </div>
                        </label>
                      </div>

                      <div>
                        <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Command <span class="text-red-500">*</span></label>
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
                          <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Work Dir</label>
                          <input
                            v-model="step.dir"
                            type="text"
                            placeholder="e.g. ./frontend"
                            class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-xs outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                          />
                        </div>
                        <div>
                          <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Timeout</label>
                          <input
                            v-model="step.timeout"
                            type="text"
                            placeholder="e.g. 10m"
                            class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-xs outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                          />
                        </div>
                        <div>
                          <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Retry</label>
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
                        <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Step Secrets</label>
                        <MultiSelect
                          v-model="(step.secrets as string[])"
                          :options="stepSecretOptions"
                          placeholder="Inherit from pipeline..."
                        />
                        <p class="text-xs text-muted-foreground/70 mt-0.5">Authorize secrets for this step only. Injected as env vars at runtime.</p>
                      </div>

                      <div v-if="step.uses === 'ssh'" class="space-y-3">
                        <p class="text-xs font-medium text-muted-foreground uppercase tracking-wider">
                          SSH targets <span class="text-red-500">*</span>
                          <span class="normal-case font-normal text-muted-foreground/80"> — at least one of Host or Labels</span>
                        </p>
                        <div class="grid grid-cols-2 gap-3">
                          <div>
                            <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">SSH Host</label>
                            <input
                              v-model="step.host"
                              type="text"
                              placeholder="e.g. prod-server-01, prod-server-02"
                              class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-xs outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                            />
                          </div>
                          <div>
                            <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Host Labels</label>
                            <input
                              v-model="step.host_labels"
                              type="text"
                              placeholder="e.g. prod, web"
                              class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-xs outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                            />
                          </div>
                        </div>
                        <p class="text-xs text-muted-foreground">
                          Combined as a union (OR): named hosts plus any host matching a listed label. Multiple values: comma-separated.
                        </p>
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

                <UiButton
                  variant="outline"
                  size="sm"
                  class="w-full"
                  @click="addStep"
                >
                  <Plus class="h-3.5 w-3.5" />
                  Add Step
                </UiButton>
                </template>
              </div>
              <!-- Advanced section -->
              <details class="group/adv rounded-xl border border-dashed border-border/80 bg-muted/10 px-3 py-2.5" :open="pipelineFormHasAdvanced">
                <summary class="flex items-center gap-3 cursor-pointer list-none select-none py-0.5 [&::-webkit-details-marker]:hidden">
                  <span class="h-px flex-1 bg-border/80 transition-colors duration-300 group-hover/adv:bg-foreground/20" />
                  <span class="inline-flex items-center gap-1.5 shrink-0 text-xs font-medium uppercase tracking-[0.14em] text-muted-foreground transition-colors duration-300 group-hover/adv:text-foreground">
                    Advanced
                    <ChevronDown class="h-3.5 w-3.5 transition-transform duration-300 ease-out group-open/adv:rotate-180" />
                    <span v-if="pipelineFormHasAdvanced" class="text-xs font-normal normal-case tracking-normal text-muted-foreground/60">
                      ({{ [form.runner_timeout && 1, form.concurrency_group && 1, form.var_ids.length > 0 && 1, form.secret_ids.length > 0 && 1, form.env.length > 0 && 1, form.input_defs.length > 0 && 1].filter(Boolean).length }} configured)
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
                      v-model="form.runner_timeout"
                      type="text"
                      placeholder="e.g. 60m"
                      class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                    />
                  </div>
                  <div>
                    <label class="type-label block mb-1.5">Concurrency Group</label>
                    <input
                      v-model="form.concurrency_group"
                      type="text"
                      placeholder="e.g. prod"
                      class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                    />
                  </div>
                  <div v-if="form.concurrency_group">
                    <label class="type-label block mb-1.5">Concurrency Mode</label>
                    <select
                      v-model="form.concurrency_mode"
                      class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                    >
                      <option value="">discard-old (default)</option>
                      <option value="queue">queue</option>
                      <option value="discard-new">discard-new</option>
                    </select>
                  </div>
                  <div v-if="form.concurrency_group && form.concurrency_mode === 'queue'">
                    <label class="type-label block mb-1.5">Max Queue</label>
                    <input
                      v-model.number="form.concurrency_max_queue"
                      type="number"
                      min="1"
                      max="100"
                      class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                    />
                  </div>
                  <div v-if="form.concurrency_group && form.concurrency_mode === 'queue'">
                    <label class="type-label block mb-1.5">Queue Timeout (seconds)</label>
                    <input
                      v-model.number="form.concurrency_queue_timeout_seconds"
                      type="number"
                      min="60"
                      class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                      placeholder="1800"
                    />
                  </div>
                </div>

                <div class="grid grid-cols-2 gap-4">
                  <div>
                    <label class="type-label block mb-1.5">Variables</label>
                    <MultiSelect
                      v-model="form.var_ids"
                      :options="variableOptions"
                      placeholder="Select platform variables..."
                    />
                    <p class="mt-1 text-xs text-muted-foreground">
                      Bind platform variables (Infrastructure → Variables). Available as <code class="font-mono">$NAME</code> in every step.
                    </p>
                  </div>
                  <div>
                    <label class="type-label block mb-1.5">Authorized Secrets</label>
                    <MultiSelect
                      v-model="form.secret_ids"
                      :options="secretOptions"
                      placeholder="Select secrets..."
                    />
                    <p class="mt-1 text-xs text-muted-foreground">
                      Explicit binding via <code class="font-mono">pipeline_secrets</code>.
                      Required for secrets without Team share (same team only).
                    </p>
                  </div>
                </div>

                <!-- Env (pipeline-level key-value pairs, user-defined) -->
                <div class="rounded-xl border border-border/70 bg-muted/20 p-4 space-y-3">
                  <div class="flex items-center justify-between">
                    <div class="flex items-center gap-2">
                      <GripVertical class="h-4 w-4 text-muted-foreground" />
                      <span class="text-sm font-medium">Env</span>
                      <span class="text-xs text-muted-foreground">(user-defined, only for this pipeline)</span>
                    </div>
                    <button
                      type="button"
                      class="inline-flex items-center gap-1 text-xs font-medium text-primary hover:text-primary/80 transition-colors"
                      @click="addEnv"
                    >
                      <Plus class="h-3 w-3" /> Add
                    </button>
                  </div>
                  <div v-if="form.env.length === 0" class="text-xs text-muted-foreground">
                    No custom env vars defined. Add key-value pairs specific to this pipeline.
                  </div>
                  <div v-for="(pair, ei) in form.env" :key="ei" class="flex items-center gap-3">
                    <input
                      v-model="form.env[ei].name"
                      type="text"
                      placeholder="KEY"
                      class="flex-1 rounded-lg border border-border bg-input px-2.5 py-1.5 text-xs font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                    />
                    <span class="text-xs text-muted-foreground">=</span>
                    <input
                      v-model="form.env[ei].value"
                      type="text"
                      placeholder="value"
                      class="flex-1 rounded-lg border border-border bg-input px-2.5 py-1.5 text-xs font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                    />
                    <button type="button" class="text-muted-foreground hover:text-destructive transition-colors" @click="removeEnv(ei)">
                      <X class="h-3.5 w-3.5" />
                    </button>
                  </div>
                </div>


                <!-- Input parameters (manual trigger only) -->
                <div v-if="form.trigger_type === 'manual'" class="rounded-xl border border-border/70 bg-muted/20 p-4 space-y-3">
                  <div class="flex items-center justify-between">
                    <div class="flex items-center gap-2">
                      <GripVertical class="h-4 w-4 text-muted-foreground" />
                      <span class="text-sm font-medium">Params</span>
                      <span class="text-xs text-muted-foreground">(optional)</span>
                    </div>
                    <button
                      type="button"
                      class="inline-flex items-center gap-1 text-xs font-medium text-primary hover:text-primary/80 transition-colors"
                      @click="addInputDef"
                    >
                      <Plus class="h-3 w-3" /> Add
                    </button>
                  </div>

                  <p class="text-xs text-muted-foreground leading-relaxed">
                    Each param is available as <code class="mono-chip">$INPUT_&lt;NAME&gt;</code>.
                    Enable <span class="font-medium text-foreground/80">Masking</span> to redact logs/audit and encrypt the value in the job DB.
                  </p>

                  <div v-if="form.input_defs.length === 0" class="text-xs text-muted-foreground">
                    No input parameters defined. Pipelines without inputs run with fixed configuration.
                  </div>

                  <div
                    v-for="(inp, idx) in form.input_defs"
                    :key="idx"
                    class="rounded-lg border border-border/50 bg-background px-3 py-2.5 space-y-2"
                  >
                    <div class="flex items-start gap-2">
                      <div class="flex-1 min-w-0 space-y-1">
                        <label class="type-label block text-[11px]">
                          Name <span class="text-red-500">*</span>
                        </label>
                        <input
                          v-model="inp.name"
                          type="text"
                          placeholder="e.g. version"
                          class="w-full rounded-md border border-border bg-muted/50 px-2.5 py-1.5 text-sm font-mono outline-none transition-colors focus:border-ring/40"
                        />
                      </div>
                      <button
                        type="button"
                        class="inline-flex h-7 w-7 mt-5 items-center justify-center rounded-md text-muted-foreground hover:text-destructive transition-colors shrink-0"
                        @click="removeInputDef(idx)"
                      >
                        <Trash2 class="h-3.5 w-3.5" />
                      </button>
                    </div>
                    <div class="grid grid-cols-1 sm:grid-cols-2 gap-2">
                      <div class="space-y-1">
                        <label class="type-label block text-[11px]">Description</label>
                        <input
                          v-model="inp.description"
                          type="text"
                          placeholder="Shown in trigger dialog"
                          class="w-full rounded-md border border-border bg-muted/50 px-2.5 py-1.5 text-xs outline-none transition-colors focus:border-ring/40"
                        />
                      </div>
                      <div class="space-y-1">
                        <label class="type-label block text-[11px]">Default</label>
                        <input
                          v-model="inp.default"
                          :type="inp.masking ? 'password' : 'text'"
                          placeholder="Optional default"
                          class="w-full rounded-md border border-border bg-muted/50 px-2.5 py-1.5 text-xs font-mono outline-none transition-colors focus:border-ring/40"
                          :autocomplete="inp.masking ? 'new-password' : 'off'"
                        />
                      </div>
                    </div>
                    <div class="flex flex-wrap items-center gap-4 pt-0.5">
                      <label class="flex items-center gap-1.5 text-xs text-muted-foreground cursor-pointer">
                        <input v-model="inp.required" type="checkbox" class="rounded border-border" />
                        Required
                      </label>
                      <label class="flex items-center gap-1.5 text-xs text-muted-foreground cursor-pointer">
                        <input v-model="inp.masking" type="checkbox" class="rounded border-border" />
                        <span>Masking</span>
                        <span class="text-muted-foreground/60">(logs + DB encrypt)</span>
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
              <UiButton variant="outline" @click="showDialog = false">Cancel</UiButton>
              <UiButton :disabled="saving || !form.name.trim() || form.steps.length === 0" @click="handleSave">
                {{ saving ? 'Saving...' : (editingId ? 'Update Pipeline' : 'Create Pipeline') }}
              </UiButton>
            </div>
          </template>

          <!-- YAML tab -->
          <template v-else>
            <div class="flex flex-col min-h-0" style="height: calc(85vh - 10rem)">
              <label class="type-label block mb-1.5 shrink-0">Pipeline YAML</label>
              <div class="flex w-full min-h-0 flex-1 rounded-xl border border-border bg-input overflow-hidden transition-colors focus-within:border-ring/40 focus-within:ring-2 focus-within:ring-ring/15 mb-3">
                <!-- Line numbers -->
                <pre
                  id="yaml-linenums"
                  class="select-none text-right shrink-0 bg-muted/40 text-xs font-mono leading-[1.7] text-muted-foreground/60 py-2.5 pl-2 pr-2 overflow-y-auto [scrollbar-width:none] [-ms-overflow-style:none] [&::-webkit-scrollbar]:hidden"
                  aria-hidden="true"
                  v-text="yamlLineNumbers"
                />
                <textarea
                  ref="yamlEditorRef"
                  v-model="yamlContent"
                  spellcheck="false"
                  placeholder="name: hello-world&#10;steps:&#10;  - name: greet&#10;    run: echo Hello"
                  class="flex-1 bg-transparent px-3 py-2.5 text-sm font-mono outline-none leading-[1.7] resize-none"
                  @scroll="syncYamlScroll"
                />
              </div>

              <div class="flex gap-2 shrink-0 mb-3">
                <UiButton size="sm" variant="outline" :disabled="yamlValidating || !yamlContent.trim()" @click="handleValidate">
                  <CheckCircle class="h-3.5 w-3.5" />
                  {{ yamlValidating ? 'Validating...' : 'Validate' }}
                </UiButton>
              </div>

              <!-- Validation result -->
              <div v-if="yamlResult" class="space-y-2">
                <!-- Success -->
                <div v-if="yamlResult.valid" class="rounded-xl border border-emerald-500/30 bg-emerald-500/5 px-4 py-3 space-y-3">
                  <div class="flex items-center gap-2 text-emerald-600 dark:text-emerald-400">
                    <CheckCircle class="h-4 w-4" />
                    <span class="text-sm font-medium">YAML is valid</span>
                  </div>
                  <div v-if="yamlResult.warnings.length > 0" class="space-y-1">
                    <div v-for="(w, i) in yamlResult.warnings" :key="'w'+i"
                         class="flex items-start gap-1.5 text-xs text-muted-foreground">
                      <AlertTriangle class="h-3 w-3 shrink-0 mt-0.5 text-amber-500" />
                      {{ w }}
                    </div>
                  </div>
                  <UiButton size="sm" @click="importYamlToForm">Import to Form</UiButton>
                </div>

                <!-- Errors -->
                <div v-else class="rounded-xl border border-destructive/30 bg-destructive/5 px-4 py-3 space-y-2">
                  <div class="flex items-center gap-2 text-destructive">
                    <AlertCircle class="h-4 w-4" />
                    <span class="text-sm font-medium">Validation failed</span>
                  </div>
                  <ul class="space-y-1.5">
                    <li v-for="(e, i) in yamlResult.errors" :key="i" class="text-xs">
                      <span class="font-mono text-destructive/80">{{ e.field }}</span>
                      <span class="text-muted-foreground"> — {{ e.message }}</span>
                    </li>
                  </ul>
                  <div v-if="yamlResult.warnings.length > 0" class="space-y-1 pt-1 border-t border-border/60">
                    <div v-for="(w, i) in yamlResult.warnings" :key="'w'+i"
                         class="flex items-start gap-1.5 text-xs text-muted-foreground">
                      <AlertTriangle class="h-3 w-3 shrink-0 mt-0.5 text-amber-500" />
                      {{ w }}
                    </div>
                  </div>
                </div>
              </div>

              <!-- Import result (success or warnings) -->
              <!-- Success — all items imported cleanly -->
              <div
                v-if="yamlImportSuccess"
                class="rounded-xl border border-emerald-500/30 bg-emerald-500/5 px-4 py-3 mt-3"
              >
                <div class="flex items-start gap-2">
                  <CheckCircle class="h-4 w-4 shrink-0 mt-0.5 text-emerald-500" />
                  <div class="flex-1">
                    <p class="text-sm font-medium text-emerald-600 dark:text-emerald-400">
                      All fields imported successfully
                    </p>
                    <p class="text-xs text-muted-foreground mt-0.5">
                      No missing references detected. You can review the result on the Form tab and save the pipeline.
                    </p>
                  </div>
                  <button
                    type="button"
                    class="shrink-0 inline-flex items-center gap-1.5 rounded-lg border border-emerald-500/30 bg-emerald-500/10 px-3 py-1.5 text-xs font-medium text-emerald-600 hover:bg-emerald-500/20 transition-colors"
                    @click="dialogMode = 'form'"
                  >
                    <FileText class="h-3 w-3" />
                    Review in Form
                  </button>
                </div>
              </div>

              <!-- Warnings — some items couldn't be mapped -->
              <div
                v-if="yamlImportWarnings.length > 0"
                class="rounded-xl border border-amber-500/30 bg-amber-500/5 px-4 py-3 mt-3"
              >
                <div class="flex items-start gap-2 mb-2">
                  <AlertTriangle class="h-4 w-4 shrink-0 mt-0.5 text-amber-500" />
                  <div class="flex-1">
                    <p class="text-sm font-medium text-amber-600 dark:text-amber-400">
                      {{ yamlImportWarnings.length }} item(s) could not be fully imported
                    </p>
                    <p class="text-xs text-muted-foreground mt-0.5">
                      The form has been populated with the available data. Review and complete the missing fields on the Form tab.
                    </p>
                  </div>
                  <button
                    type="button"
                    class="shrink-0 inline-flex items-center gap-1.5 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-1.5 text-xs font-medium text-amber-600 hover:bg-amber-500/20 transition-colors"
                    @click="dialogMode = 'form'"
                  >
                    <FileText class="h-3 w-3" />
                    Review in Form
                  </button>
                </div>
                <ul class="ml-6 space-y-1.5">
                  <li
                    v-for="(w, idx) in yamlImportWarnings"
                    :key="idx"
                    class="text-xs text-muted-foreground list-disc"
                  >{{ w }}</li>
                </ul>
              </div>

              <!-- Quick reference -->
              <details class="group mt-3">
                <summary class="text-xs text-muted-foreground cursor-pointer hover:text-foreground transition-colors">
                  Quick YAML Reference
                </summary>
                <pre class="mt-2 rounded-lg bg-muted p-3 text-xs font-mono text-muted-foreground overflow-x-auto">name: hello-world
description: A complete pipeline with all supported features
team: default                     # team slug (omit → default); alias: owner_group

# ── Trigger ── (pick ONE)
on:
  manual:                         # Manual trigger with dynamic params
    inputs:
      version:
        description: Version to deploy
        required: true
      deploy_token:
        description: One-time deploy token
        required: true
        masking: true             # redact logs/audit; encrypt in job DB
      env_name:
        description: Target environment
        default: staging

  # webhook:                      # Webhook trigger
  #   path: abc123def456

  # cron:                         # Cron trigger
  #   schedule: "0 2 * * *"

# ── Runner ──
runner:
  labels: [linux-x64]             # optional; runner must have ALL listed labels (AND)
  timeout: 60m

# ── Concurrency ── (mode defaults to discard-old)
# Default — omit mode entirely (equivalent to discard-old):
concurrency:
  group: prod

# Queue mode — jobs wait in FIFO queue until runner is free:
# concurrency:
#   group: prod
#   mode: queue
#   max_queue: 5
#   queue_timeout: 30m

# Discard-new — reject new triggers while a job is active:
# concurrency:
#   group: prod
#   mode: discard-new

# ── Pipeline-level env vars (non-sensitive, inline key-value) ──
env:
  APP_NAME: myapp
  LOG_LEVEL: info

# ── Platform Variables (must exist in Manager → Variables first) ──
variables:
  - BUILD_CACHE_DIR
  - NOTIFY_EMAIL

# ── Pipeline-level secrets (must exist in Manager → Secrets first) ──
# Explicit bind — required unless the secret has Team share enabled.
# Same team only. Prefer step secrets: / registry_secret for scoping.
secrets:
  - DOCKER_REGISTRY_PASS
  - SLACK_WEBHOOK_URL

steps:
  # ── Basic shell step ──
  - name: Checkout
    run: |
      echo "==> Cloning repo..."
      git clone --depth 1 https://github.com/example/hello-world.git /tmp/build
      echo "Clone complete"

  # ── Build with advanced options ──
  - name: Build
    run: cargo build --release --manifest-path=apps/$INPUT_ENV_NAME/Cargo.toml
    work_dir: /tmp/build
    timeout: 10m
    retry: 2

  # ── Run inside a container on the runner (Docker engine) ──
  - name: Unit Tests
    uses: docker
    image: node:20-bookworm
    work_dir: ./frontend
    volumes:
      - "/data/cache:/cache"
    run: |
      npm ci
      npm test
    timeout: 15m

  # ── Private image from ACR Personal Edition ──
  # Secret "acr-personal" is created in Infrastructure → Secrets (encrypted).
  # Value JSON includes provider/registry/username/password — not this YAML.
  - name: Run Private Image
    uses: docker
    image: crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com/ns/app:1.0
    registry_secret: acr-personal
    run: |
      echo "pulled private image"
    timeout: 10m

  # ── Docker push via registry_secret (shell engine auto login/logout) ──
  - name: Docker Push
    uses: shell
    registry_secret: acr-personal
    run: |
      docker push crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com/ns/myapp:$INPUT_VERSION
      echo "CACHE_DIR=$BUILD_CACHE_DIR"
    timeout: 5m

  # ── Build + push orchestration (see example/pipelines/acr-personal-build-push.yml) ──
  - name: Build Image
    uses: shell
    run: |
      docker build -t crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com/ns/hello:$INPUT_VERSION .
    timeout: 10m
  - name: Push Image
    uses: shell
    registry_secret: acr-personal
    run: |
      docker push crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com/ns/hello:$INPUT_VERSION
    timeout: 15m

  # ── SSH deploy (host and/or host_labels — at least one required; union) ──
  - name: Deploy
    uses: ssh
    host:
      - prod-server-01
      - prod-server-02
    host_labels:
      - prod
      - web
    run: |
      docker tag myapp:$INPUT_VERSION myapp:latest
      docker-compose up -d
      curl -X POST -d '{"text":"deploy complete"}' "$SLACK_WEBHOOK_URL"
    continue-on-error: true

  # ── Step-level secret (only this step sees GIT_SSH_KEY) ──
  - name: Clone Private Repo
    secrets: [GIT_SSH_KEY]
    run: |
      echo "$GIT_SSH_KEY" > /tmp/key && chmod 600 /tmp/key
      GIT_SSH_COMMAND="ssh -i /tmp/key -o StrictHostKeyChecking=no" \
        git clone --depth 1 git@github.com:org/private-repo.git /tmp/private
      rm -f /tmp/key
    timeout: 5m

# ────────────────────────────────────────────────────
# Variable resolution inside steps:
#   $INPUT_*      → dynamic trigger parameters
#   $APP_NAME     → pipeline-level env vars
#   $BUILD_CACHE_DIR → platform Variables (bound via UI or YAML)
#   $DOCKER_REGISTRY_PASS → pipeline-level Secrets
#   $GIT_SSH_KEY  → step-level Secret (only for Clone Private Repo)
#
# Secrets & Variables must first exist in Manager UI:
#   Infrastructure → Secrets   (add DOCKER_REGISTRY_PASS, etc.)
#   Infrastructure → Variables (add BUILD_CACHE_DIR, etc.)
# ────────────────────────────────────────────────────</pre>
              </details>

              <div class="flex justify-end gap-2 pt-2 shrink-0">
                <UiButton variant="outline" @click="showDialog = false">Cancel</UiButton>
              </div>
            </div>
          </template>
        </div>
      </div>
    </Teleport>
  </div>
</template>
