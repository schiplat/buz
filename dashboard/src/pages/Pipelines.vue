<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { useRouter } from 'vue-router'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import {
  Plus, X, Play, Trash2,
  ChevronDown, ChevronRight, Loader2, Puzzle, Pencil, Minus, Snowflake, CircleHelp,
} from '@lucide/vue'
import {
  pipelinesApi,
  type Pipeline,
  type PipelineInputDef,
} from '@/api/pipelines'
import { findMissingRequiredInputs, normalizeInputDefsForForm } from '@/utils/pipelineInputs'
import { parseApiError, type FormattedApiError } from '@/utils/error'
import { jobsApi, type Job } from '@/api/jobs'
import { environmentsApi, type Environment } from '@/api/environments'
import { cn } from '@/lib/utils'
import { parseUtc } from '@/utils/time'
import TablePagination from '@/components/ui/TablePagination.vue'
import PipelineYamlEditorDialog from '@/components/pipeline/PipelineYamlEditorDialog.vue'
import { useAuthStore } from '@/stores/auth'
import { useClientSort } from '@/composables/useClientSort'
import { useClientPagination } from '@/composables/useClientPagination'
import { useAppDialog } from '@/composables/useAppDialog'

const router = useRouter()
const auth = useAuthStore()
const dialog = useAppDialog()

const pipelines = ref<Pipeline[]>([])
const loading = ref(true)

const searchQuery = ref('')
const filterTrigger = ref('')
const filterSource = ref('')
const filterActive = ref('')
/** Status chip filter from the summary bar: '' = all, otherwise a job status
 *  bucket; the pipeline table then shows only pipelines whose latest job
 *  falls in that bucket (24h chips additionally check recency). */
const statusFilter = ref<'' | 'running' | 'approval' | 'queued' | 'passed' | 'failed'>('')

function toggleStatusFilter(key: 'running' | 'approval' | 'queued' | 'passed' | 'failed') {
  statusFilter.value = statusFilter.value === key ? '' : key
}

const DAY_MS = 24 * 60 * 60 * 1000

function pipelineInStatusBucket(p: Pipeline, key: typeof statusFilter.value): boolean {
  const status = p.latest_job_status
  if (!status) return false
  // Buckets must mirror the summary-bar counts exactly:
  // Approval = pending + waiting_approval (both need a human/Runner action).
  if (key === 'running') return status === 'running'
  if (key === 'approval') return status === 'pending' || status === 'waiting_approval'
  if (key === 'queued') return status === 'queued'
  // 24h buckets: pipeline rows only know the latest job's status; the row's
  // updated_at advances when its jobs change, so require it to be recent.
  const recent = p.updated_at != null && statNow.value - parseUtc(p.updated_at) < DAY_MS
  if (!recent) return false
  if (key === 'passed') return status === 'success'
  return status === 'failed'
}

const filteredPipelines = computed(() => {
  let source = pipelines.value.filter((p) => auth.matchesActiveTeam(p.team_id))
  if (filterTrigger.value) source = source.filter((p) => p.trigger_type === filterTrigger.value)
  if (filterSource.value) source = source.filter((p) => p.source === filterSource.value)
  if (filterActive.value) source = source.filter((p) => (filterActive.value === 'active' ? p.is_active : !p.is_active))
  if (statusFilter.value) source = source.filter((p) => pipelineInStatusBucket(p, statusFilter.value))
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

function formatSuccessRate(p: Pipeline): string {
  if (p.job_total == null || p.job_total === 0 || p.success_rate == null) return '—'
  return `${Math.round(p.success_rate * 100)}%`
}

function successRateTitle(p: Pipeline): string {
  if (p.job_total == null || p.job_total === 0) return 'No builds yet'
  return `${p.job_success ?? 0} successful / ${p.job_total} total builds`
}

const yamlEditorRef = ref<InstanceType<typeof PipelineYamlEditorDialog> | null>(null)

function openCreate() {
  yamlEditorRef.value?.openCreate()
}

async function openEdit(p: Pipeline) {
  await yamlEditorRef.value?.openEdit(p)
}

async function onYamlSaved() {
  await loadPipelines()
}

const triggeringId = ref<string | null>(null)
const triggerTarget = ref<Pipeline | null>(null)
type TriggerInputPair = {
  key: string
  value: string
  required?: boolean
  masking?: boolean
  type?: string
  pattern?: string
  options?: string[]
}
const triggerInputPairs = ref<TriggerInputPair[]>([])
/** True when the opened trigger dialog has declared on.manual.inputs. */
const triggerHasInputDefs = ref(false)
/** Shown when POST /trigger fails (e.g. 422 YAML validation). */
const triggerError = ref<FormattedApiError | null>(null)
/** Field names flagged by the last validation error — highlighted red in the dialog. */
const triggerErrorFields = computed<Set<string>>(() => {
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
const declaredTriggerEnvironments = computed(() => {
  const declared = new Set(triggerTarget.value?.environments || [])
  return environments.value.filter((env) => declared.has(env.slug))
})

const selectedTriggerEnv = computed(() =>
  environments.value.find((e) => e.slug === triggerEnvironment.value) || null,
)

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

// ── Jobs status summary bar ──
const allJobs = ref<Job[]>([])
const statNow = ref(Date.now())
let statTimer: ReturnType<typeof setInterval> | null = null

const teamFilteredJobs = computed(() =>
  allJobs.value.filter(j => auth.matchesActiveTeam(j.team_id)),
)
const runningJobs = computed(() =>
  teamFilteredJobs.value.filter(j => j.status === 'running'),
)
const pendingJobs = computed(() =>
  teamFilteredJobs.value.filter(j => j.status === 'pending' || j.status === 'waiting_approval'),
)
const queuedJobs = computed(() =>
  teamFilteredJobs.value.filter(j => j.status === 'queued'),
)
const totalJobCount = computed(() => teamFilteredJobs.value.length)
const hasActiveJobs = computed(() =>
  runningJobs.value.length > 0 || pendingJobs.value.length > 0 || queuedJobs.value.length > 0,
)
const activeCount = computed(() =>
  runningJobs.value.length + pendingJobs.value.length + queuedJobs.value.length,
)

/** Terminal jobs in the last 24h, for the aggregate pass/fail counts. */
const passedJobs24h = computed(() =>
  teamFilteredJobs.value.filter(
    (j) =>
      j.status === 'success' &&
      j.end_time != null &&
      statNow.value - parseUtc(j.end_time) < DAY_MS,
  ),
)
const failedJobs24h = computed(() =>
  teamFilteredJobs.value.filter(
    (j) =>
      j.status === 'failed' &&
      j.end_time != null &&
      statNow.value - parseUtc(j.end_time) < DAY_MS,
  ),
)

async function fetchJobs() {
  try {
    allJobs.value = await jobsApi.list()
  } catch {
    /* ignore */
  }
}

function startStatsTimer() {
  if (statTimer) return
  statTimer = setInterval(() => {
    statNow.value = Date.now()
  }, 5000)
}

function stopStatsTimer() {
  if (statTimer) {
    clearInterval(statTimer)
    statTimer = null
  }
}

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

onMounted(async () => {
  await loadPipelines()
  fetchJobs()
  startStatsTimer()
})

onUnmounted(() => {
  stopStatsTimer()
})

async function handleTrigger(p: Pipeline) {
  if (triggeringId.value) return
  triggerDebug.value = false
  triggerVerbose.value = false
  triggerEnvironment.value = ''
  triggerError.value = null
  showAdhocParams.value = false
  await loadEnvironments()
  let defs: PipelineInputDef[] = Array.isArray(p.input_defs) ? p.input_defs : []
  try {
    const detail = await pipelinesApi.get(p.id)
    if (Array.isArray(detail.input_defs)) defs = detail.input_defs
  } catch {
    /* use list payload */
  }
  triggerTarget.value = p
  triggerHasInputDefs.value = defs.length > 0
  triggerInputPairs.value = defs.length > 0
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
}

function toggleAdhocParams() {
  showAdhocParams.value = !showAdhocParams.value
  if (showAdhocParams.value && triggerInputPairs.value.length === 0) {
    triggerInputPairs.value = [{ key: '', value: '' }]
  }
}

function addTriggerInputPair() {
  triggerInputPairs.value.push({ key: '', value: '' })
}

function removeTriggerInputPair(idx: number) {
  triggerInputPairs.value.splice(idx, 1)
}

async function doTrigger(p: Pipeline) {
  if (triggeringId.value) return
  // Client-side required check: empty value == not filled. Blocks submission
  // before the request so the dialog can highlight the offending fields.
  const missing = findMissingRequiredInputs(triggerInputPairs.value)
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
      items: [{ field: 'environment', message: 'environment is required' }],
    }
    return
  }
  triggeringId.value = p.id
  triggerError.value = null
  try {
    const inputs: Record<string, string> = {}
    for (const pair of triggerInputPairs.value) {
      // Empty values are not sent so the backend merges declared defaults.
      if (pair.key.trim() && (pair.value ?? '').trim()) inputs[pair.key.trim()] = pair.value
    }
    const result = await pipelinesApi.trigger(
      p.id,
      inputs,
      triggerDebug.value,
      triggerVerbose.value,
      triggerEnvironment.value || null,
    )
    triggerTarget.value = null
    triggerInputPairs.value = []
    triggerHasInputDefs.value = false
    showAdhocParams.value = false
    triggerDebug.value = false
    triggerVerbose.value = false
    triggerEnvironment.value = ''
    router.push(`/jobs/${result.job_id}`)
  } catch (e) {
    console.error(e)
    triggerError.value = parseApiError(e, 'Trigger failed')
  } finally {
    triggeringId.value = null
  }
}

function cancelTriggerDialog() {
  triggerTarget.value = null
  triggerInputPairs.value = []
  triggerHasInputDefs.value = false
  showAdhocParams.value = false
  triggerDebug.value = false
  triggerVerbose.value = false
  triggerError.value = null
  triggeringId.value = null
}

async function handleDelete(p: Pipeline) {
  if (!(await dialog.confirm(`Delete pipeline "${p.name}"?`, {
    title: 'Delete pipeline',
    confirmLabel: 'Delete',
    danger: true,
  }))) return
  try {
    await pipelinesApi.delete(p.id)
    await loadPipelines()
  } catch (e) {
    console.error(e)
    await dialog.alert('Delete failed')
  }
}

async function handleToggleActive(p: Pipeline) {
  if (p.is_active) {
    const choice = await dialog.confirmChoice(
      `Disable pipeline "${p.name}"? All trigger paths (manual, webhook, cron, git hook) will reject new runs.`,
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
      const result = await pipelinesApi.disable(p.id, choice.checked)
      p.is_active = false
      if (result.cancelled_count > 0) {
        await dialog.alert(
          `Pipeline disabled — cancelled ${result.cancelled_count} in-flight job(s).`,
          { title: 'Pipeline disabled' },
        )
      }
    } catch (e) {
      console.error(e)
      await dialog.alert('Disable failed')
    }
  } else {
    const ok = await dialog.confirm(`Enable pipeline "${p.name}"?`, {
      title: 'Enable pipeline',
      confirmLabel: 'Enable',
    })
    if (!ok) return
    try {
      const result = await pipelinesApi.enable(p.id)
      p.is_active = result.is_active
    } catch (e) {
      console.error(e)
      await dialog.alert('Enable failed')
    }
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

    <!-- ── Jobs status summary bar (aggregate; click a status to filter) ── -->
    <div
      v-if="!loading && allJobs.length > 0"
      class="mb-4 flex flex-wrap items-center gap-1 rounded-xl border border-border/60 bg-card px-2.5 py-2"
      role="group"
      aria-label="Run status summary — click a status to filter the table"
    >
      <button
        type="button"
        class="inline-flex items-center gap-1.5 rounded-md px-2 py-0.5 text-[11px] transition-colors"
        :class="statusFilter === 'running' ? 'bg-muted font-medium text-foreground ring-1 ring-border' : 'text-foreground/80 hover:bg-muted/60'"
        :aria-pressed="statusFilter === 'running'"
        title="Show pipelines with a running job"
        @click="toggleStatusFilter('running')"
      >
        <span class="h-1.5 w-1.5 rounded-full bg-amber-500" aria-hidden="true"></span>
        <span class="font-mono text-[12px] font-medium tabular-nums" :class="runningJobs.length === 0 && 'text-muted-foreground/50'">{{ runningJobs.length }}</span>
        <span>Running</span>
      </button>
      <button
        type="button"
        class="inline-flex items-center gap-1.5 rounded-md px-2 py-0.5 text-[11px] transition-colors"
        :class="statusFilter === 'approval' ? 'bg-muted font-medium text-foreground ring-1 ring-border' : 'text-foreground/80 hover:bg-muted/60'"
        :aria-pressed="statusFilter === 'approval'"
        title="Show pipelines waiting for approval"
        @click="toggleStatusFilter('approval')"
      >
        <span class="h-1.5 w-1.5 rounded-full bg-violet-500" aria-hidden="true"></span>
        <span class="font-mono text-[12px] font-medium tabular-nums" :class="pendingJobs.length === 0 && 'text-muted-foreground/50'">{{ pendingJobs.length }}</span>
        <span>Approval</span>
      </button>
      <button
        type="button"
        class="inline-flex items-center gap-1.5 rounded-md px-2 py-0.5 text-[11px] transition-colors"
        :class="statusFilter === 'queued' ? 'bg-muted font-medium text-foreground ring-1 ring-border' : 'text-foreground/80 hover:bg-muted/60'"
        :aria-pressed="statusFilter === 'queued'"
        title="Show pipelines with queued jobs"
        @click="toggleStatusFilter('queued')"
      >
        <span class="h-1.5 w-1.5 rounded-full bg-sky-500" aria-hidden="true"></span>
        <span class="font-mono text-[12px] font-medium tabular-nums" :class="queuedJobs.length === 0 && 'text-muted-foreground/50'">{{ queuedJobs.length }}</span>
        <span>Queued</span>
      </button>
      <span class="mx-2 hidden h-3.5 w-px bg-border/60 sm:block" aria-hidden="true"></span>
      <button
        type="button"
        class="inline-flex items-center gap-1.5 rounded-md px-2 py-0.5 text-[11px] transition-colors"
        :class="statusFilter === 'passed' ? 'bg-muted font-medium text-foreground ring-1 ring-border' : 'text-foreground/80 hover:bg-muted/60'"
        :aria-pressed="statusFilter === 'passed'"
        title="Show pipelines that passed in the last 24h"
        @click="toggleStatusFilter('passed')"
      >
        <span class="h-1.5 w-1.5 rounded-full bg-emerald-500" aria-hidden="true"></span>
        <span class="font-mono text-[12px] font-medium tabular-nums" :class="passedJobs24h.length === 0 && 'text-muted-foreground/50'">{{ passedJobs24h.length }}</span>
        <span>Passed · 24h</span>
      </button>
      <button
        type="button"
        class="inline-flex items-center gap-1.5 rounded-md px-2 py-0.5 text-[11px] transition-colors"
        :class="statusFilter === 'failed' ? 'bg-muted font-medium text-foreground ring-1 ring-red-500/40' : 'text-foreground/80 hover:bg-muted/60'"
        :aria-pressed="statusFilter === 'failed'"
        title="Show pipelines that failed in the last 24h"
        @click="toggleStatusFilter('failed')"
      >
        <span class="h-1.5 w-1.5 rounded-full bg-red-500" aria-hidden="true"></span>
        <span class="font-mono text-[12px] font-medium tabular-nums" :class="failedJobs24h.length === 0 ? 'text-muted-foreground/50' : 'text-red-600'">{{ failedJobs24h.length }}</span>
        <span>Failed · 24h</span>
      </button>
      <span class="ml-auto inline-flex items-center gap-2.5 pl-3 text-[10.5px] text-muted-foreground">
        <span><strong class="font-mono font-medium text-foreground/70">{{ activeCount }}</strong> active · <strong class="font-mono font-medium text-foreground/70">{{ totalJobCount }}</strong> total</span>
        <button
          v-if="statusFilter"
          type="button"
          class="inline-flex items-center gap-1 rounded-md border border-border/60 px-1.5 py-0.5 text-[10px] transition-colors hover:bg-muted"
          @click="statusFilter = ''"
        >
          Clear filter <X class="h-2.5 w-2.5" />
        </button>
        <span
          class="inline-flex items-center gap-1 font-mono text-[8.5px] uppercase tracking-[0.08em] text-muted-foreground/60"
          title="Auto-refreshes every 5s"
        >
          <span class="h-1 w-1 rounded-full bg-emerald-500" aria-hidden="true"></span>
          Live
        </span>
      </span>
    </div>

    <div v-if="loading" class="surface-card p-8 text-center type-meta">Loading...</div>

    <template v-else>
      <div v-if="filteredPipelines.length === 0" class="surface-card p-8 text-center type-meta">
        {{ pipelines.length === 0 ? 'No pipelines created yet' : 'No pipelines in the selected team' }}
      </div>

      <div v-else class="surface-card overflow-hidden">
        <div class="px-5 py-3 border-b border-border/30 flex items-center gap-3 flex-wrap">
          <input
            v-model="searchQuery"
            type="text"
            placeholder="Search pipelines…"
            class="w-full max-w-52 rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-hidden transition-colors focus:border-primary/50 focus:ring-1 focus:ring-primary/10"
          />
          <select v-model="filterTrigger" class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-hidden transition-colors focus:border-primary/50">
            <option value="">All triggers</option>
            <option value="manual">Manual</option>
            <option value="webhook">Webhook</option>
            <option value="cron">Cron</option>
          </select>
          <select v-model="filterSource" class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-hidden transition-colors focus:border-primary/50">
            <option value="">All sources</option>
            <option value="form">Form</option>
            <option value="yaml">YAML</option>
          </select>
          <select v-model="filterActive" class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-hidden transition-colors focus:border-primary/50">
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
                    <span class="font-mono text-xs opacity-50">{{ sortIndicator('name') }}</span>
                  </button>
                </th>
                <th>
                  <button type="button" class="inline-flex items-center gap-1 hover:text-foreground transition-colors" @click="toggleSort('trigger_type')">
                    Trigger
                    <span class="font-mono text-xs opacity-50">{{ sortIndicator('trigger_type') }}</span>
                  </button>
                </th>
                <th>R-Labels</th>
                <th>
                  <button type="button" class="inline-flex items-center gap-1 hover:text-foreground transition-colors" @click="toggleSort('last_build')">
                    Last Build
                    <span class="font-mono text-xs opacity-50">{{ sortIndicator('last_build') }}</span>
                  </button>
                </th>
                <th>
                  <button type="button" class="inline-flex items-center gap-1 hover:text-foreground transition-colors" @click="toggleSort('success_rate')" title="Successful jobs ÷ total builds">
                    Rate
                    <span class="font-mono text-xs opacity-50">{{ sortIndicator('success_rate') }}</span>
                  </button>
                </th>
                <th>
                  <button type="button" class="inline-flex items-center gap-1 hover:text-foreground transition-colors" @click="toggleSort('is_active')">
                    Active
                    <span class="font-mono text-xs opacity-50">{{ sortIndicator('is_active') }}</span>
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
                      class="font-mono text-[12px] font-medium leading-snug text-foreground/90 text-left hover:text-primary hover:underline underline-offset-2 transition-colors cursor-pointer break-all"
                      :title="p.name"
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
                        class="text-[10px] uppercase tracking-wide text-violet-600/90"
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
                    <UiButton
                      variant="ghost"
                      size="sm"
                      @click="openEdit(p)"
                      title="Edit YAML"
                    >
                      <Pencil class="h-3.5 w-3.5" />
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
        <div class="absolute inset-0 bg-black/30 backdrop-blur-xs" @click="cancelTriggerDialog()" />
        <div class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6 max-h-[90vh] overflow-y-auto">
          <h2 class="text-[15px] font-semibold tracking-tight">Trigger Pipeline</h2>
          <p v-if="triggerHasInputDefs" class="mt-1.5 text-[12px] text-muted-foreground leading-relaxed">
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

          <div v-if="triggerHasInputDefs" class="mt-5 space-y-2.5">
            <div
              v-for="(pair, idx) in triggerInputPairs"
              :key="idx"
              class="rounded-lg border px-3 py-2.5 transition-colors"
              :class="triggerErrorFields.has(pair.key) ? 'border-destructive/60 bg-destructive/5' : 'border-border/40 bg-muted/20'"
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
                :class="triggerErrorFields.has(pair.key) ? 'border-destructive' : 'border-border/60'"
                :disabled="triggeringId !== null"
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
              <input
                v-else
                v-model="pair.value"
                :type="pair.masking ? 'password' : pair.type === 'number' ? 'number' : 'text'"
                :placeholder="pair.required ? 'required' : 'enter value (leave empty for default)'"
                class="w-full rounded-lg border bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50"
                :class="triggerErrorFields.has(pair.key) ? 'border-destructive' : 'border-border/60'"
                :disabled="triggeringId !== null"
                :autocomplete="pair.masking ? 'new-password' : 'off'"
              />
            </div>
            <p v-if="triggerErrorFields.size > 0" class="text-[12px] text-destructive">
              {{ triggerErrorFields.size }} invalid field(s) — fix them and trigger again.
            </p>
          </div>

          <div v-else class="mt-5">
            <button
              type="button"
              class="inline-flex items-center gap-1 text-[12px] font-medium text-muted-foreground hover:text-foreground transition-colors"
              :disabled="triggeringId !== null"
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
              <div
                v-for="(pair, idx) in triggerInputPairs"
                :key="idx"
                class="flex items-center gap-2"
              >
                <input
                  v-model="pair.key"
                  placeholder="key"
                  class="flex-1 rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50"
                  :disabled="triggeringId !== null"
                />
                <input
                  v-model="pair.value"
                  type="text"
                  placeholder="value"
                  class="flex-2 rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50"
                  :disabled="triggeringId !== null"
                />
                <button
                  type="button"
                  class="shrink-0 rounded-lg p-2 text-muted-foreground hover:bg-muted hover:text-destructive transition-colors"
                  :disabled="triggeringId !== null"
                  @click="removeTriggerInputPair(idx)"
                >
                  <Trash2 class="h-4 w-4" />
                </button>
              </div>
              <button
                type="button"
                class="flex items-center gap-1 text-[12px] font-medium text-primary hover:text-primary/80 transition-colors"
                :disabled="triggeringId !== null"
                @click="addTriggerInputPair"
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
              :disabled="triggeringId !== null"
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
              <input v-model="triggerVerbose" type="checkbox" :disabled="triggeringId !== null" />
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
              <input v-model="triggerDebug" type="checkbox" :disabled="triggeringId !== null" />
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
            <UiButton variant="outline" size="sm" @click="cancelTriggerDialog()">Cancel</UiButton>
            <UiButton size="sm" @click="doTrigger(triggerTarget!)" :disabled="triggeringId !== null || environmentLoadFailed || !triggerEnvironment || declaredTriggerEnvironments.length === 0">
              <Loader2 v-if="triggeringId === triggerTarget?.id" class="mr-1.5 h-4 w-4 animate-spin" />
              <Play v-else class="mr-1.5 h-4 w-4" /> Run
            </UiButton>
          </div>
        </div>
      </div>
    </Teleport>

    <PipelineYamlEditorDialog ref="yamlEditorRef" @saved="onYamlSaved" />
  </div>
</template>
