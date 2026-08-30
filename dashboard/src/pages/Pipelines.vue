<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { useRouter } from 'vue-router'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import {
  Plus, X, Play, Trash2,
  ChevronDown, ChevronRight, Loader2, Puzzle, Pencil, Minus, Snowflake, Clock, CircleHelp,
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

const selectedTriggerEnv = computed(() =>
  environments.value.find((e) => e.slug === triggerEnvironment.value) || null,
)

async function loadEnvironments() {
  try {
    environments.value = await environmentsApi.list()
  } catch {
    environments.value = []
  }
}

// ── Jobs stats panel ──
const allJobs = ref<Job[]>([])
const statsExpanded = ref(false)
const statNow = ref(Date.now())
let statTimer: ReturnType<typeof setInterval> | null = null

const teamFilteredJobs = computed(() =>
  allJobs.value.filter(j => auth.matchesActiveTeam(j.team_id)),
)
const runningJobs = computed(() =>
  teamFilteredJobs.value.filter(j => j.status === 'running'),
)
const pendingJobs = computed(() =>
  teamFilteredJobs.value.filter(j => j.status === 'pending'),
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
    allJobs.value = allJobs.value.filter(j => j.id !== jobId)
  } catch (e) {
    console.error(e)
  }
}

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

function toggleStats() {
  statsExpanded.value = !statsExpanded.value
  if (statsExpanded.value) {
    stopStatsTimer()
    statTimer = setInterval(() => { statNow.value = Date.now() }, 1000)
  } else {
    stopStatsTimer()
    startStatsTimer()
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
  void loadEnvironments()
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

        <div class="ml-auto flex items-center gap-2">
          <span class="inline-flex items-baseline gap-1">
            <span class="text-sm font-semibold tabular-nums text-muted-foreground/80">{{ activeCount }}</span>
            <span class="text-xs text-muted-foreground/40 leading-none">of</span>
            <span class="text-sm tabular-nums text-muted-foreground/60">{{ totalJobCount }}</span>
          </span>
          <button class="w-5 h-5 flex items-center justify-center">
            <ChevronDown v-if="statsExpanded" class="h-4 w-4 text-muted-foreground/40" />
            <ChevronRight v-else class="h-4 w-4 text-muted-foreground/40" />
          </button>
        </div>
      </div>

      <template v-if="statsExpanded">
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
        <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="cancelTriggerDialog()" />
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
                  class="mb-0.5 inline-block rounded bg-destructive/15 px-1.5 py-0.5 font-mono text-[11px] text-destructive"
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
                  class="rounded px-1.5 py-0.5 text-[10px] font-medium uppercase tracking-wide bg-violet-500/10 text-violet-600 border border-violet-500/20"
                >masking</span>
                <span
                  v-if="pair.type"
                  class="rounded px-1.5 py-0.5 text-[10px] font-medium bg-sky-500/10 text-sky-600 border border-sky-500/20"
                >{{ pair.type }}</span>
              </div>
              <select
                v-if="pair.type === 'choice' || pair.type === 'boolean'"
                v-model="pair.value"
                class="w-full rounded-lg border bg-background px-3 py-2 text-sm outline-none focus:border-primary/50"
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
                class="w-full rounded-lg border bg-background px-3 py-2 text-sm outline-none focus:border-primary/50"
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
                  class="flex-1 rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50"
                  :disabled="triggeringId !== null"
                />
                <input
                  v-model="pair.value"
                  type="text"
                  placeholder="value"
                  class="flex-[2] rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50"
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
            <label class="mb-1.5 block text-[12px] font-medium">Environment</label>
            <select
              v-model="triggerEnvironment"
              class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50"
              :disabled="triggeringId !== null"
            >
              <option value="">Default (no environment)</option>
              <option v-for="e in environments" :key="e.id" :value="e.slug">
                {{ e.name }} ({{ e.slug }}){{ e.protection_enabled ? ' — protected' : '' }}
              </option>
            </select>
            <p
              v-if="selectedTriggerEnv?.protection_enabled"
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
            <UiButton size="sm" @click="doTrigger(triggerTarget!)" :disabled="triggeringId !== null">
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
