<script setup lang="ts">
import { ref, computed, onMounted, reactive, watch } from 'vue'
import { useRouter } from 'vue-router'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import { Plus, X, Play, Trash2, GripVertical, Code, FileText, CheckCircle, AlertTriangle, AlertCircle, ChevronDown, ChevronRight, Loader2, Puzzle, Pencil, Minus, Snowflake, Copy, Check } from '@lucide/vue'
import { pipelinesApi, type Pipeline, type StepDef, type YamlValidationResult, type PipelineInputDef, type YamlParsedData } from '@/api/pipelines'
import { secretsApi } from '@/api/secrets'
import { variablesApi } from '@/api/variables'
import { teamsApi, type Team } from '@/api/teams'
import { cn } from '@/lib/utils'
import UiPagination from '@/components/ui/UiPagination.vue'
import MultiSelect from '@/components/ui/MultiSelect.vue'
import { useAuthStore } from '@/stores/auth'

const router = useRouter()
const auth = useAuthStore()

const pipelines = ref<Pipeline[]>([])
const loading = ref(true)
const availableSecrets = ref<{ id: string; name: string; owner_group: string }[]>([])
const availableVariables = ref<{ id: string; name: string; value: string }[]>([])
const availableTeams = ref<Team[]>([])

/** Resolve team_id for create/update: picker value, or sole membership. */
function ensureTeamId(): string {
  if (auth.showTeamPicker) return form.value.team_id.trim()
  if (form.value.team_id.trim()) return form.value.team_id.trim()
  return auth.soleTeamId
}

const pipelinePage = ref(1)
const pageSize = 15

const filteredPipelines = computed(() =>
  pipelines.value.filter((p) => auth.matchesActiveTeam(p.team_id)),
)

const pagedPipelines = computed(() => {
  const start = (pipelinePage.value - 1) * pageSize
  return filteredPipelines.value.slice(start, start + pageSize)
})

watch(() => auth.activeTeamId, () => { pipelinePage.value = 1 })

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
const triggerInputPairs = ref<{ key: string; value: string; required?: boolean }[]>([])

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

onMounted(async () => {
  await loadPipelines()
  try {
    availableSecrets.value = await secretsApi.list()
  } catch { /* ignore */ }
  try {
    availableVariables.value = await variablesApi.list()
  } catch { /* ignore */ }
  try {
    availableTeams.value = await teamsApi.list()
  } catch { /* ignore */ }
})

const variableOptions = computed(() =>
  availableVariables.value.map(v => ({ id: v.id, label: v.name }))
)
const secretOptions = computed(() =>
  availableSecrets.value.map(s => ({
    id: s.id,
    label: s.owner_group ? `${s.name} · ${s.owner_group}` : s.name,
  }))
)

async function loadPipelines() {
  loading.value = true
  try {
    pipelines.value = await pipelinesApi.list()
    pipelinePage.value = 1
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
  form.value = { name: '', description: '', trigger_type: 'manual', webhook_path: genRandomPath(), cron_expr: '', runner_labels: 'linux-x64', runner_timeout: '', concurrency_group: '', owner_group: '', team_id: auth.preferredTeamId, var_ids: [], secret_ids: [], env: [], steps: [], input_defs: [] }
  yamlContent.value = ''
  yamlResult.value = null
  yamlImportWarnings.value = []
  yamlImportSuccess.value = false
  expandedSteps.clear()
  showDialog.value = true
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
      owner_group: detail.owner_group || '',
      team_id: detail.team_id || '',
      var_ids: detail.var_ids || [],
      secret_ids: detail.secret_ids || [],
      env: [],
      steps: (detail.tasks || []).map((t: any) => ({
        name: t.name || '',
        run: t.command_template || '',
        uses: t.exec_type && t.exec_type !== 'local' ? t.exec_type : undefined,
        image: t.image || undefined,
        volumes: Array.isArray(t.volumes) ? t.volumes.join(', ') : (t.volumes || undefined),
        host: t.host_id ? t.host_id : undefined,
        host_labels: t.host_labels || undefined,
        dir: t.work_dir || undefined,
        timeout: t.timeout_seconds ? `${t.timeout_seconds}s` : undefined,
        retry: t.retry_count || undefined,
        continue_on_error: t.fail_strategy === 'continue' ? true : undefined,
      })),
      input_defs: detail.input_defs || [],
    }
    // Expand all steps
    for (let i = 0; i < form.value.steps.length; i++) expandedSteps.add(i)
  } catch (e) {
    console.error('Failed to load pipeline detail', e)
  }
}

function addStep() {
  const idx = form.value.steps.length
  form.value.steps.push({ name: '', run: '', uses: 'ssh' })
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
  form.value.input_defs.push({ name: '', description: '', required: false, default: '' })
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

function importYamlToForm() {
  try {
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
      const resolved: string[] = []
      for (const name of parsed.variables) {
        const found = availableVariables.value.find(v => v.name === name)
        if (found) {
          resolved.push(found.id)
        } else {
          warnings.push(`Variable "${name}" not found in Manager. Create it in Infrastructure → Variables first.`)
        }
      }
      resolvedVarIds = resolved
    }

    // Pipeline-level secrets: look up by name → resolve to IDs
    let resolvedSecretIds: string[] = []
    if (parsed.secrets && parsed.secrets.length > 0) {
      const resolved: string[] = []
      for (const name of parsed.secrets) {
        const found = availableSecrets.value.find(s => s.name === name)
        if (found) {
          resolved.push(found.id)
        } else {
          warnings.push(`Secret "${name}" not found in Manager. Create it in Infrastructure → Secrets first.`)
        }
      }
      resolvedSecretIds = resolved
    }

    // Step-level secrets: the form StepDef has no per-step secrets field — always warn
    for (let i = 0; i < parsed.steps.length; i++) {
      const s = parsed.steps[i]
      if (s.secrets && s.secrets.length > 0) {
        const stepLabel = s.name || `Step ${i + 1}`
        for (const sn of s.secrets) {
          warnings.push(`Step "${stepLabel}" has per-step secret "${sn}". The form does not yet support step-level secrets — add it manually to pipeline-level Secrets instead.`)
        }
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
        uses: s.uses && s.uses !== 'local' ? s.uses : undefined,
        image: s.image || undefined,
        volumes: s.volumes
          ? s.volumes.split(',').map((v: string) => v.trim()).filter(Boolean)
          : undefined,
        host: normalizeHost(s.host),
        host_labels: normalizeHost(s.host_labels),
        dir: s.dir || undefined,
        timeout: s.timeout || undefined,
        retry: s.retry || undefined,
        continue_on_error: s.continue_on_error || undefined,
      })),
      input_defs: (parsed.trigger_config?.inputs || []).map(inp => ({
        name: inp.name || '',
        description: inp.description || '',
        required: inp.required || false,
        default: inp.default || '',
      })),
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
        const rawVolumes: string = (s as any).volumes || ''
        const volumesArray = isDocker && rawVolumes.trim()
          ? rawVolumes.split(',').map((v: string) => v.trim()).filter(Boolean)
          : undefined
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
      input_defs: form.value.trigger_type === 'manual' && form.value.input_defs.length > 0
        ? form.value.input_defs
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

  const defs: PipelineInputDef[] = (p as any).input_defs
  if (Array.isArray(defs) && defs.length > 0) {
    // Pipeline has defined input parameters — show dialog
    triggerTarget.value = p
    triggerInputPairs.value = defs.map(d => ({ key: d.name, value: d.default || '', required: d.required }))
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

    <div v-if="loading" class="surface-card p-8 text-center type-meta">Loading...</div>

    <template v-else>
      <div v-if="filteredPipelines.length === 0" class="surface-card p-8 text-center type-meta">
        {{ pipelines.length === 0 ? 'No pipelines created yet' : 'No pipelines in the selected team' }}
      </div>

      <div v-else class="surface-card data-table-scroll">
        <table class="data-table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Trigger</th>
              <th>R-Labels</th>
              <th>Last Build</th>
              <th title="Successful jobs ÷ total builds">Rate</th>
              <th>Active</th>
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
                    class="inline-flex items-center gap-1 rounded-md border border-border/60 bg-muted/40 px-2 py-0.5 text-[11px] text-muted-foreground"
                    title="Params"
                  >
                    <Puzzle class="h-3 w-3" />
                    Params
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
                    class="text-[10px] text-muted-foreground pl-3.5 tabular-nums"
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
                  class="block text-[10px] text-muted-foreground mt-0.5 tabular-nums"
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
        <UiPagination v-model:page="pipelinePage" :total="filteredPipelines.length" :page-size="pageSize" />
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
          </p>

          <div class="space-y-2 mb-6">
            <div
              v-for="(pair, idx) in triggerInputPairs"
              :key="idx"
              class="flex items-center gap-2"
            >
              <div
                class="flex-1 rounded-md border border-border/40 bg-muted/30 px-3 py-2 text-sm font-mono text-muted-foreground select-none flex items-center gap-1"
              >
                {{ pair.key }}
                <span v-if="pair.required" class="text-red-400 text-[13px] font-sans leading-none">*</span>
                <span v-else class="text-[10px] text-muted-foreground/50 font-sans">(optional)</span>
              </div>
              <input
                v-model="pair.value"
                :placeholder="pair.required ? 'required' : 'enter value (leave empty for default)'"
                class="flex-[2] rounded-md border border-border bg-muted/50 px-3 py-2 text-sm"
                :disabled="triggeringId !== null"
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
                </div>
              </div>

              <div v-if="form.trigger_type === 'webhook'" class="rounded-xl border border-border/70 bg-muted/20 p-4 space-y-3">
                <div class="flex items-center gap-2">
                  <Code class="h-4 w-4 text-muted-foreground" />
                  <span class="text-sm font-medium">Webhook Configuration</span>
                </div>
                <div>
                  <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">URL Path</label>
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
                  <p class="text-[11px] text-muted-foreground mt-1.5">
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
                  <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Schedule (cron expression)</label>
                  <input
                    v-model="form.cron_expr"
                    type="text"
                    placeholder="0 2 * * *"
                    class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-sm font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                  />
                  <p class="text-[11px] text-muted-foreground mt-1.5">
                    Standard 5‑field cron syntax: minute hour day month weekday.
                  </p>
                  <details class="mt-2">
                    <summary class="text-[11px] text-muted-foreground cursor-pointer hover:text-foreground">Quick reference</summary>
                    <div class="mt-1.5 grid grid-cols-2 gap-x-4 gap-y-0.5 text-[11px] font-mono text-muted-foreground">
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
                <p class="mt-1 text-[10px] text-muted-foreground">
                  Same-team secrets from Infrastructure → Secrets are auto-injected. Use Authorized Secrets for cross-team bindings.
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
                  class="w-full inline-flex items-center justify-center gap-2 rounded-xl border border-dashed border-border px-3 py-2.5 text-[13px] text-muted-foreground hover:border-foreground/25 hover:bg-muted/20 hover:text-foreground transition-colors"
                  @click="addStep"
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
                        <span v-if="step.uses" class="mono-chip text-[10px]">{{ step.uses }}</span>
                        <span v-if="step.continue_on_error" class="mono-chip text-[10px] text-amber-600 bg-amber-500/10">no-fail</span>
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
                        <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">
                          Image <span class="text-red-500">*</span>
                        </label>
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
                        <input
                          :value="(step as any).volumes || ''"
                          @input="(e) => (step as any).volumes = (e.target as HTMLInputElement).value"
                          type="text"
                          placeholder="/data/logs:/var/log/app, /cfg:/etc/app/config"
                          class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-sm font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                        />
                        <p class="text-[10px] text-muted-foreground/70 mt-0.5">Comma-separated, each entry “host_path:container_path”. All paths must exist on the runner host.</p>
                      </div>

                      <div>
                        <label class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-1 block">Command <span class="text-red-500">*</span></label>
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
                  <span class="inline-flex items-center gap-1.5 shrink-0 text-[11px] font-medium uppercase tracking-[0.14em] text-muted-foreground transition-colors duration-300 group-hover/adv:text-foreground">
                    Advanced
                    <ChevronDown class="h-3.5 w-3.5 transition-transform duration-300 ease-out group-open/adv:rotate-180" />
                    <span v-if="pipelineFormHasAdvanced" class="text-[10px] font-normal normal-case tracking-normal text-muted-foreground/60">
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
                </div>

                <div class="grid grid-cols-2 gap-4">
                  <div>
                    <label class="type-label block mb-1.5">Variables</label>
                    <MultiSelect
                      v-model="form.var_ids"
                      :options="variableOptions"
                      placeholder="Select platform variables..."
                    />
                    <p class="mt-1 text-[10px] text-muted-foreground">
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
                      @click="addEnv"
                    >
                      <Plus class="h-3 w-3" /> Add
                    </button>
                  </div>
                  <div v-if="form.env.length === 0" class="text-[11px] text-muted-foreground">
                    No custom env vars defined. Add key-value pairs specific to this pipeline.
                  </div>
                  <div v-for="(pair, ei) in form.env" :key="ei" class="flex items-center gap-3">
                    <input
                      v-model="form.env[ei].name"
                      type="text"
                      placeholder="KEY"
                      class="flex-1 rounded-lg border border-border bg-input px-2.5 py-1.5 text-xs font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                    />
                    <span class="text-[10px] text-muted-foreground">=</span>
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
                      <span class="text-[11px] text-muted-foreground">(optional)</span>
                    </div>
                    <button
                      type="button"
                      class="inline-flex items-center gap-1 text-[11px] font-medium text-primary hover:text-primary/80 transition-colors"
                      @click="addInputDef"
                    >
                      <Plus class="h-3 w-3" /> Add
                    </button>
                  </div>

                  <div v-if="form.input_defs.length === 0" class="text-[11px] text-muted-foreground">
                    No input parameters defined. Pipelines without inputs run with fixed configuration.
                  </div>

                  <div
                    v-for="(inp, idx) in form.input_defs"
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
                        @click="removeInputDef(idx)"
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
                  class="select-none text-right shrink-0 bg-muted/40 text-[11px] font-mono leading-[1.7] text-muted-foreground/60 py-2.5 pl-2 pr-2 overflow-y-auto [scrollbar-width:none] [-ms-overflow-style:none] [&::-webkit-scrollbar]:hidden"
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
                    <p class="text-[11px] text-muted-foreground mt-0.5">
                      No missing references detected. You can review the result on the Form tab and save the pipeline.
                    </p>
                  </div>
                  <button
                    type="button"
                    class="shrink-0 inline-flex items-center gap-1.5 rounded-lg border border-emerald-500/30 bg-emerald-500/10 px-3 py-1.5 text-[11px] font-medium text-emerald-600 hover:bg-emerald-500/20 transition-colors"
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
                    <p class="text-[11px] text-muted-foreground mt-0.5">
                      The form has been populated with the available data. Review and complete the missing fields on the Form tab.
                    </p>
                  </div>
                  <button
                    type="button"
                    class="shrink-0 inline-flex items-center gap-1.5 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-1.5 text-[11px] font-medium text-amber-600 hover:bg-amber-500/20 transition-colors"
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
      env_name:
        description: Target environment
        default: staging

  # webhook:                      # Webhook trigger
  #   path: abc123def456

  # cron:                         # Cron trigger
  #   schedule: "0 2 * * *"

# ── Runner ──
runner:
  labels: [linux-x64]
  timeout: 60m

# ── Concurrency ──
concurrency: prod

# ── Pipeline-level env vars (non-sensitive, inline key-value) ──
env:
  APP_NAME: myapp
  LOG_LEVEL: info

# ── Platform Variables (must exist in Manager → Variables first) ──
variables:
  - BUILD_CACHE_DIR
  - NOTIFY_EMAIL

# ── Pipeline-level secrets (must exist in Manager → Secrets first) ──
secrets:
  - DOCKER_REGISTRY_PASS
  - SLACK_WEBHOOK_URL

steps:
  # ── Basic local step ──
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

  # ── Docker push (uses pipeline-level secret; local engine) ──
  - name: Docker Push
    run: |
      echo "$DOCKER_REGISTRY_PASS" | docker login -u bot --password-stdin
      docker push myapp:$INPUT_VERSION
      echo "CACHE_DIR=$BUILD_CACHE_DIR"
    timeout: 5m

  # ── SSH deploy to multiple hosts ──
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
