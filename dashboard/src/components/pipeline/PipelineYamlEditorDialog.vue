<script setup lang="ts">
import { ref, computed, nextTick, onMounted } from 'vue'
import {
  X,
  CheckCircle,
  AlertTriangle,
  AlertCircle,
  Loader2,
  FileCode2,
  BookOpenText,
  GripVertical,
} from '@lucide/vue'
import UiButton from '@/components/ui/UiButton.vue'
import { pipelinesApi, type Pipeline, type YamlValidationResult } from '@/api/pipelines'
import { normalizeInputDefsForForm } from '@/utils/pipelineInputs'
import { normalizeConfigRender } from '@/utils/configForm'
import { secretsApi } from '@/api/secrets'
import { variablesApi } from '@/api/variables'
import { serializePipelineFormToYaml } from '@/utils/pipelineYamlSerialize'
import {
  severityBadgeClass,
  severityWordClass,
  splitDiagnosticSegments,
} from '@/utils/severityHighlight'
import { splitWarningField } from '@/utils/pipelineYamlLint'
import PipelineYamlCodeEditor from '@/components/pipeline/PipelineYamlCodeEditor.vue'
import { cn } from '@/lib/utils'
import { useAppDialog } from '@/composables/useAppDialog'
import { DEFAULT_MARKETPLACE_URL, getMarketplaceUrl } from '@/api/health'

const emit = defineEmits<{
  saved: [payload: { pipelineId: string | null }]
}>()

const dialog = useAppDialog()
/** First-visit coachmark for New Pipeline → Quick YAML Reference. */
const REF_GUIDE_KEY = 'buz_guide_new_pipeline_yaml_ref'

const open = ref(false)
const dialogTitle = ref('New Pipeline')
const editingId = ref<string | null>(null)

// Marketplace link from Manager config (`marketplace_url`), falling back to
// the public marketplace when unset or when the health probe is unreachable.
const marketplaceHref = ref(DEFAULT_MARKETPLACE_URL)
onMounted(async () => {
  marketplaceHref.value = await getMarketplaceUrl()
})
const saving = ref(false)
/** Dialog body tab — matches Secrets/Variables segmented control style. */
const dialogTab = ref<'editor' | 'reference'>('editor')
/** First-visit tip pointing at Quick YAML Reference (create only). */
const showRefGuide = ref(false)
const refTabBtnRef = ref<HTMLButtonElement | null>(null)

function hasSeenRefGuide(): boolean {
  try {
    return localStorage.getItem(REF_GUIDE_KEY) === '1'
  } catch {
    return true
  }
}

function dismissRefGuide() {
  try {
    localStorage.setItem(REF_GUIDE_KEY, '1')
  } catch {
    // ignore — private browsing, storage full, etc.
  }
  showRefGuide.value = false
}

function selectDialogTab(tab: 'editor' | 'reference') {
  dialogTab.value = tab
  if (tab === 'reference' && showRefGuide.value) {
    dismissRefGuide()
  }
}

const yamlContent = ref('')
const yamlFilename = ref('')
const yamlPathHint = ref('')
/** Content ETag from last load — sent on Save for If-Match concurrency. */
const yamlEtag = ref<string | null>(null)
/** Manager `pipeline.yaml_scan_dir` from list API — used for live path preview. */
const yamlScanDir = ref('')
/** Existing YAML files under scan dir (for filename conflict checks). */
const existingYamlFiles = ref<
  Array<{
    filename: string
    yaml_path: string
    pipeline_id?: string | null
    pipeline_name?: string | null
  }>
>([])
const yamlResult = ref<YamlValidationResult | null>(null)
const yamlValidating = ref(false)
const yamlFileLocked = ref(false)
const codeEditorRef = ref<InstanceType<typeof PipelineYamlCodeEditor> | null>(null)

/** Slugified basename that will be written (empty when input blank). */
const resolvedFilename = computed(() => {
  const raw = yamlFilename.value.trim()
  return raw ? slugifyYamlFilename(raw) : ''
})

/** Live storage path shown under Filename (create / migrate); locked edit uses server path. */
const displayStoragePath = computed(() => {
  if (yamlFileLocked.value && yamlPathHint.value) {
    return yamlPathHint.value
  }
  const name = resolvedFilename.value
  const scan = yamlScanDir.value.trim()
  if (!scan && !name) return ''
  if (!scan) return name
  const base = scan.replace(/[/\\]+$/, '')
  if (!name) return `${base}/`
  return `${base}/${name}`
})

/** Another orchestration file already uses this basename (create / migrate). */
const filenameConflict = computed(() => {
  if (yamlFileLocked.value) return null
  const name = resolvedFilename.value
  if (!name) return null
  const lower = name.toLowerCase()
  const hit = existingYamlFiles.value.find((f) => f.filename.toLowerCase() === lower)
  if (!hit) return null
  // Same pipeline id (edge case) is not a conflict.
  if (editingId.value && hit.pipeline_id && hit.pipeline_id === editingId.value) return null
  return hit
})

/** Floating diagnostics panel (right of editor). */
const floatVisible = ref(false)
/** Fingerprint of the error set the user dismissed — stay hidden until errors change. */
const floatDismissedKey = ref<string | null>(null)
/** Viewport position for the float panel (`fixed`); null = place near editor on next show. */
const floatPos = ref<{ left: number; top: number } | null>(null)
const floatDragging = ref(false)
const floatPanelRef = ref<HTMLElement | null>(null)
const editorShellRef = ref<HTMLElement | null>(null)
let floatDragCleanup: (() => void) | null = null

const availableSecrets = ref<{ id: string; name: string; owner_group: string; kind?: string }[]>([])
const availableVariables = ref<{ id: string; name: string; value: string }[]>([])

const secretNames = computed(() => availableSecrets.value.map((s) => s.name))
const variableNames = computed(() => availableVariables.value.map((v) => v.name))

const floatErrors = computed(() => yamlResult.value?.errors ?? [])
const floatWarnings = computed(() => yamlResult.value?.warnings ?? [])
/** Show float for hard errors and/or soft warnings (warnings-only still surfaces). */
const hasFloatIssues = computed(
  () => floatErrors.value.length > 0 || floatWarnings.value.length > 0,
)
const floatWarningsOnly = computed(
  () => floatErrors.value.length === 0 && floatWarnings.value.length > 0,
)
const showFloatReopenChip = computed(
  () => hasFloatIssues.value && !floatVisible.value && floatDismissedKey.value !== null,
)

function diagnosticsKey(result: YamlValidationResult | null): string {
  if (!result) return ''
  const errs = (result.errors || []).map((e) => `${e.field}\0${e.message}`).join('\n')
  const warns = (result.warnings || []).join('\n')
  if (!errs && !warns) return ''
  return `${errs}\n--\n${warns}`
}

/** Normalize to a safe basename matching Manager `validate_yaml_filename` (no spaces). */
function slugifyYamlFilename(raw: string): string {
  const trimmed = (raw || '').trim()
  if (!trimmed) return 'pipeline.yml'
  const lower = trimmed.toLowerCase()
  let ext = '.yml'
  let stem = trimmed
  if (lower.endsWith('.yaml')) {
    ext = '.yaml'
    stem = trimmed.slice(0, -5)
  } else if (lower.endsWith('.yml')) {
    ext = '.yml'
    stem = trimmed.slice(0, -4)
  }
  // Stem: letters, digits, '-' / '_' only — no '.' (reserved for extension).
  const slug =
    stem
      .toLowerCase()
      .replace(/[^a-z0-9_-]+/g, '-')
      .replace(/-+/g, '-')
      .replace(/^-+|-+$/g, '') || 'pipeline'
  return `${slug}${ext}`
}

function suggestYamlFilename(pipelineName: string): string {
  return slugifyYamlFilename(pipelineName || 'pipeline')
}

function close() {
  open.value = false
  dialogTab.value = 'editor'
  showRefGuide.value = false
  floatVisible.value = false
  floatDismissedKey.value = null
  floatPos.value = null
  floatDragging.value = false
  floatDragCleanup?.()
  floatDragCleanup = null
}

/** Place the float near the editor's top-right when no user drag position yet. */
function ensureFloatPos() {
  if (floatPos.value) return
  const shell = editorShellRef.value
  const panelW = 352 // ~22rem
  if (shell) {
    const r = shell.getBoundingClientRect()
    const w = Math.min(panelW, Math.max(240, r.width - 24))
    floatPos.value = {
      left: Math.max(8, r.right - w - 12),
      top: Math.max(8, r.top + 12),
    }
    return
  }
  floatPos.value = {
    left: Math.max(8, window.innerWidth - panelW - 24),
    top: Math.max(8, Math.round(window.innerHeight * 0.18)),
  }
}

function clampFloatPos(left: number, top: number): { left: number; top: number } {
  const el = floatPanelRef.value
  const w = el?.offsetWidth ?? 352
  const h = el?.offsetHeight ?? 200
  const maxL = Math.max(8, window.innerWidth - w - 8)
  const maxT = Math.max(8, window.innerHeight - h - 8)
  return {
    left: Math.min(Math.max(8, left), maxL),
    top: Math.min(Math.max(8, top), maxT),
  }
}

function onFloatHeaderPointerDown(e: PointerEvent) {
  if ((e.target as HTMLElement).closest('[data-float-dismiss]')) return
  if (e.button !== 0) return
  ensureFloatPos()
  const startX = e.clientX
  const startY = e.clientY
  const orig = { ...(floatPos.value as { left: number; top: number }) }
  floatDragging.value = true
  floatDragCleanup?.()

  const onMove = (ev: PointerEvent) => {
    floatPos.value = clampFloatPos(orig.left + ev.clientX - startX, orig.top + ev.clientY - startY)
  }
  const onUp = () => {
    floatDragging.value = false
    window.removeEventListener('pointermove', onMove)
    window.removeEventListener('pointerup', onUp)
    window.removeEventListener('pointercancel', onUp)
    floatDragCleanup = null
  }
  window.addEventListener('pointermove', onMove)
  window.addEventListener('pointerup', onUp)
  window.addEventListener('pointercancel', onUp)
  floatDragCleanup = onUp
  e.preventDefault()
}

function onEditorValidated(result: YamlValidationResult | null) {
  yamlResult.value = result
  const key = diagnosticsKey(result)
  if (!key) {
    floatVisible.value = false
    floatDismissedKey.value = null
    return
  }
  // New / changed error set → show again; same dismissed set stays closed.
  if (floatDismissedKey.value === key) {
    floatVisible.value = false
    return
  }
  floatDismissedKey.value = null
  ensureFloatPos()
  floatVisible.value = true
  nextTick(() => ensureFloatPos())
}

function dismissFloat() {
  floatDismissedKey.value = diagnosticsKey(yamlResult.value)
  floatVisible.value = false
}

function reopenFloat() {
  floatDismissedKey.value = null
  ensureFloatPos()
  floatVisible.value = hasFloatIssues.value
}

/** Scroll the YAML editor to the offending line for an error/warning entry. */
function jumpToIssue(field: string | null | undefined, message: string) {
  codeEditorRef.value?.revealDiagnostic(field || '', message || '')
}

async function loadNameMaps() {
  const [secrets, variables, yamlFiles] = await Promise.allSettled([
    secretsApi.list(),
    variablesApi.list(),
    pipelinesApi.listYamlFiles(),
  ])
  if (secrets.status === 'fulfilled') availableSecrets.value = secrets.value
  if (variables.status === 'fulfilled') availableVariables.value = variables.value
  if (yamlFiles.status === 'fulfilled') {
    if (yamlFiles.value?.scan_dir) yamlScanDir.value = yamlFiles.value.scan_dir
    existingYamlFiles.value = Array.isArray(yamlFiles.value?.files) ? yamlFiles.value.files : []
  }
}

type VolumeRow = { host: string; container: string }

function parseVolumesToRows(raw: any): VolumeRow[] {
  if (Array.isArray(raw)) {
    return raw.map((v: string) => {
      const [host, container] = String(v).split(':')
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

async function openCreate() {
  editingId.value = null
  dialogTitle.value = 'New Pipeline'
  yamlContent.value = ''
  yamlFilename.value = ''
  yamlPathHint.value = ''
  yamlEtag.value = null
  yamlFileLocked.value = false
  yamlResult.value = null
  dialogTab.value = 'editor'
  floatVisible.value = false
  floatDismissedKey.value = null
  showRefGuide.value = false
  open.value = true
  await loadNameMaps()
  await nextTick()
  if (!hasSeenRefGuide()) {
    showRefGuide.value = true
  } else {
    codeEditorRef.value?.focus()
  }
}

async function openEdit(p: Pick<Pipeline, 'id' | 'name' | 'source'> & { yaml_path?: string | null }) {
  editingId.value = p.id
  dialogTitle.value = 'Edit Pipeline'
  yamlContent.value = ''
  yamlFilename.value = ''
  yamlPathHint.value = p.yaml_path || ''
  yamlEtag.value = null
  yamlFileLocked.value = p.source === 'yaml'
  yamlResult.value = null
  dialogTab.value = 'editor'
  floatVisible.value = false
  floatDismissedKey.value = null
  open.value = true
  await loadNameMaps()

  try {
    if (p.source === 'yaml') {
      const src = await pipelinesApi.getYamlSource(p.id)
      yamlContent.value = src.yaml
      yamlFilename.value = src.filename
      yamlPathHint.value = src.yaml_path
      yamlEtag.value = src.etag || null
      return
    }

    const detail = await pipelinesApi.get(p.id)
    // owner_group stores the team slug on the pipeline row.
    const formLike = {
      name: detail.name || '',
      description: detail.description || '',
      team: (detail.owner_group || '').trim(),
      trigger_type: detail.trigger_type || 'manual',
      webhook_path: detail.webhook_path || '',
      cron_expr: detail.cron_expr || '',
      runner_labels: (detail.runner_labels || []).join(', '),
      runner_timeout: detail.timeout_seconds ? `${detail.timeout_seconds}s` : '',
      concurrency_group: detail.concurrency_group || '',
      concurrency_mode: detail.concurrency_mode || '',
      concurrency_max_queue: detail.concurrency_max_queue ?? 3,
      concurrency_queue_timeout_seconds: detail.concurrency_queue_timeout_seconds ?? 1800,
      var_ids: detail.var_ids || [],
      secret_ids: detail.secret_ids || [],
      env: (detail.env || []).map((e: any) => ({ name: e.name, value: e.value })),
      steps: (detail.tasks || []).map((t: any) => {
        // Capability grants are locked plugin metadata. Never turn resolved task
        // metadata back into an authorable YAML field during edit/serialization.
        const { capabilities: _capabilities, uses_ref: _usesRef, marketplace: _marketplace, ...authorable } = t
        return {
        name: authorable.name || '',
        run: authorable.command_template || '',
        engine: authorable.exec_type || 'shell',
        image: authorable.image || undefined,
        volumes: parseVolumesToRows(authorable.volumes) as any,
        secrets: authorable.step_secrets ? authorable.step_secrets.split(',').map((s: string) => s.trim()).filter(Boolean) : [],
        env: (authorable.step_env && typeof authorable.step_env === 'object' && !Array.isArray(authorable.step_env))
          ? { ...authorable.step_env }
          : undefined,
        host: (authorable.host_names || authorable.host_id || '').toString().trim() || undefined,
        host_labels: authorable.host_labels || undefined,
        dir: authorable.work_dir || undefined,
        timeout: authorable.timeout_seconds ? `${authorable.timeout_seconds}s` : undefined,
        retry: authorable.retry_count || undefined,
        continue_on_error: authorable.fail_strategy === 'continue' ? true : undefined,
        privileged: authorable.privileged === true ? true : undefined,
        registry_secret: (authorable.registry_secret || '').trim() || undefined,
        config_source: (authorable.config_source || '').trim() || '',
        config_key_path: (authorable.config_key_path || '').trim() || '',
        config_dest: (authorable.config_dest || '').trim() || '',
        config_render: normalizeConfigRender(authorable.config_render),
      }
      }),
      input_defs: normalizeInputDefsForForm(detail.input_defs),
    }
    const varNames = formLike.var_ids
      .map((id: string) => availableVariables.value.find((v) => v.id === id)?.name)
      .filter((n: string | undefined): n is string => !!n)
    const secretNames = formLike.secret_ids
      .map((id: string) => availableSecrets.value.find((s) => s.id === id)?.name)
      .filter((n: string | undefined): n is string => !!n)
    yamlContent.value = serializePipelineFormToYaml(formLike as any, {
      variableNames: varNames,
      secretNames,
    })
    yamlFilename.value = suggestYamlFilename(formLike.name || p.name)
    yamlFileLocked.value = false
  } catch (e: any) {
    console.error('Failed to load pipeline for YAML edit', e)
    await dialog.alert(e?.response?.data?.message || e?.message || 'Failed to load pipeline')
  }
}

async function handleValidate() {
  if (!yamlContent.value.trim()) return
  yamlValidating.value = true
  try {
    const result = await codeEditorRef.value?.validateNow()
    if (result) yamlResult.value = result
  } finally {
    yamlValidating.value = false
  }
}

async function saveFromYaml() {
  if (!yamlContent.value.trim()) return
  if (!yamlResult.value?.valid) await handleValidate()
  if (!yamlResult.value?.valid) {
    const detail = (yamlResult.value?.errors || [])
      .map((e) => `${e.field}: ${e.message}`)
      .join('\n')
    await dialog.alert(
      detail
        ? `YAML validation failed — fix errors before saving:\n\n${detail}`
        : 'YAML is invalid — fix errors before saving.',
      { title: 'Validation failed' },
    )
    return
  }

  let filename = yamlFilename.value.trim()
    ? slugifyYamlFilename(yamlFilename.value)
    : suggestYamlFilename(yamlResult.value.name || 'pipeline')
  yamlFilename.value = filename
  if (!/\.(yml|yaml)$/i.test(filename)) {
    await dialog.alert('Filename must end with .yml or .yaml')
    return
  }
  if (filenameConflict.value) {
    const other = filenameConflict.value.pipeline_name || filenameConflict.value.filename
    await dialog.alert(`Filename conflicts with existing orchestration file "${other}". Choose another name.`)
    return
  }

  saving.value = true
  try {
    let pipelineId = editingId.value
    if (editingId.value && yamlFileLocked.value) {
      const r = await pipelinesApi.updateYamlFile(filename, {
        yaml: yamlContent.value,
        ...(yamlEtag.value ? { etag: yamlEtag.value } : {}),
      })
      pipelineId = r.pipeline_id
      yamlEtag.value = r.etag || null
    } else if (editingId.value && !yamlFileLocked.value) {
      const r = await pipelinesApi.updateYamlFile(filename, {
        yaml: yamlContent.value,
        pipeline_id: editingId.value,
        ...(yamlEtag.value ? { etag: yamlEtag.value } : {}),
      })
      pipelineId = r.pipeline_id
      yamlEtag.value = r.etag || null
    } else {
      const r = await pipelinesApi.createYamlFile({ yaml: yamlContent.value, filename })
      pipelineId = r.pipeline_id
      yamlEtag.value = r.etag || null
    }
    open.value = false
    emit('saved', { pipelineId })
  } catch (e: any) {
    console.error(e)
    const status = e?.response?.status
    const msg = e?.response?.data?.message || e?.message || 'Save failed'
    if (status === 409 && /If-Match|etag|modified/i.test(String(msg))) {
      await dialog.alert(`${msg}\n\nReload the YAML from disk and re-apply your edits before saving again.`)
    } else {
      await dialog.alert(msg)
    }
  } finally {
    saving.value = false
  }
}

defineExpose({ openCreate, openEdit, close })
</script>

<template>
  <Teleport to="body">
    <div
      v-if="open"
      class="fixed inset-0 z-50 flex items-center justify-center p-4 sm:p-6"
      @click.self="close"
    >
      <div
        class="soft-panel w-full max-w-6xl h-[min(92vh,calc(100vh-2rem))] max-h-[min(92vh,calc(100vh-2rem))] !p-0 flex flex-col overflow-hidden min-h-0"
        role="dialog"
        aria-modal="true"
      >
        <div class="flex items-center justify-between shrink-0 px-5 sm:px-6 pt-5 sm:pt-6 mb-3">
          <h2 class="text-base font-semibold tracking-tight">{{ dialogTitle }}</h2>
          <button
            type="button"
            class="inline-flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground"
            @click="close"
          >
            <X class="h-4 w-4" />
          </button>
        </div>

        <!-- Tab bar — same segmented control as Secrets / Variables -->
        <div
          class="relative flex items-center justify-between shrink-0 mx-5 sm:mx-6"
          :class="showRefGuide ? 'z-20 mb-36' : 'mb-3'"
        >
          <div class="inline-flex w-fit items-center gap-0.5 rounded-lg bg-muted/70 p-0.5">
            <button
              type="button"
              :class="
                cn(
                  'inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-[13px] font-medium transition-all cursor-pointer',
                  dialogTab === 'editor'
                    ? 'bg-background text-foreground shadow-sm'
                    : 'text-muted-foreground hover:text-foreground hover:bg-background/60',
                )
              "
              @click="selectDialogTab('editor')"
            >
              <FileCode2 class="h-3.5 w-3.5" />
              Editor
            </button>
            <div class="relative">
              <button
                ref="refTabBtnRef"
                type="button"
                :class="
                  cn(
                    'relative z-20 inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-[13px] font-medium transition-all cursor-pointer',
                    dialogTab === 'reference'
                      ? 'bg-background text-foreground shadow-sm'
                      : 'text-muted-foreground hover:text-foreground hover:bg-background/60',
                    showRefGuide && 'ring-2 ring-amber-500/50 ring-offset-2 ring-offset-background shadow-sm bg-background text-foreground',
                  )
                "
                @click="selectDialogTab('reference')"
              >
                <BookOpenText class="h-3.5 w-3.5" />
                Quick YAML Reference
              </button>

              <!-- First-visit coachmark (New Pipeline only) — points at this tab -->
              <div
                v-if="showRefGuide"
                class="absolute left-0 top-full z-30 mt-2.5 w-64 sm:w-72"
                role="status"
              >
                <div
                  class="absolute left-8 bottom-full border-x-[6px] border-x-transparent border-b-[7px] border-b-border"
                  aria-hidden="true"
                />
                <div
                  class="absolute left-8 bottom-full mb-px ml-px border-x-[5px] border-x-transparent border-b-[6px] border-b-card"
                  aria-hidden="true"
                />
                <div class="rounded-xl border border-border bg-card px-3.5 py-3 shadow-lg">
                  <p class="text-[13px] font-medium text-foreground leading-snug">
                    Start with Quick YAML Reference
                  </p>
                  <p class="mt-1 text-xs text-muted-foreground leading-relaxed">
                    Examples, field notes, and timeout / secrets syntax — open this tab anytime while authoring.
                  </p>
                  <div class="mt-2.5 flex items-center justify-end gap-2">
                    <button
                      type="button"
                      class="text-xs font-medium text-muted-foreground hover:text-foreground transition-colors px-2 py-1"
                      @click="dismissRefGuide"
                    >
                      Dismiss
                    </button>
                    <UiButton
                      size="sm"
                      class="h-7 text-xs px-2.5"
                      @click="selectDialogTab('reference')"
                    >
                      Open reference
                    </UiButton>
                  </div>
                </div>
              </div>
            </div>
          </div>
          <a
            :href="marketplaceHref"
            target="_blank"
            rel="noopener noreferrer"
            class="shrink-0 px-2 py-1.5 text-[13px] font-medium text-muted-foreground transition-colors hover:text-foreground"
          >
            Plugin Marketplace
          </a>
        </div>

        <!-- Scrollable body — keeps dialog within viewport -->
        <div class="flex flex-col min-h-0 flex-1 overflow-y-auto overscroll-contain px-5 sm:px-6">
          <div v-show="dialogTab === 'editor'" class="flex flex-col min-h-0">
          <div class="shrink-0 mb-3 space-y-1.5">
            <label class="type-label block">
              Filename <span class="text-red-500">*</span>
            </label>
            <input
              v-model="yamlFilename"
              type="text"
              :readonly="yamlFileLocked"
              placeholder="e.g. hello-world.yml"
              class="w-full rounded-xl border bg-input px-3 py-2 text-sm font-mono outline-none transition-colors disabled:opacity-70"
              :class="
                yamlFileLocked
                  ? 'opacity-70 cursor-not-allowed border-border'
                  : filenameConflict
                    ? 'border-destructive/60 focus:border-destructive focus:ring-2 focus:ring-destructive/20'
                    : 'border-border focus:border-ring/40 focus:ring-2 focus:ring-ring/15'
              "
            />
            <p class="text-[11px] text-muted-foreground leading-relaxed">
              <template v-if="displayStoragePath">
                Path
                <code class="font-mono text-foreground/80">{{ displayStoragePath }}</code>
              </template>
              <template v-else>
                Written under Manager <code class="font-mono">yaml_scan_dir</code>
              </template>
              <template v-if="yamlFileLocked"> — file locked to this pipeline</template>
              <template v-else-if="editingId && !yamlFileLocked"> — Save migrates this pipeline to file authority</template>
            </p>
            <p
              v-if="filenameConflict"
              class="flex items-start gap-1.5 text-[11px] text-destructive leading-relaxed"
            >
              <AlertTriangle class="h-3.5 w-3.5 shrink-0 mt-0.5" />
              <span>
                Filename conflicts with existing file
                <code class="font-mono">{{ filenameConflict.filename }}</code>
                <template v-if="filenameConflict.pipeline_name">
                  (pipeline
                  <span class="font-medium">{{ filenameConflict.pipeline_name }}</span>)
                </template>
                — choose another name.
              </span>
            </p>
          </div>

          <label class="type-label block mb-1.5 shrink-0">Pipeline YAML <span class="text-red-500">*</span></label>
          <div
            ref="editorShellRef"
            class="relative w-full h-[min(50vh,28rem)] min-h-[16rem] shrink-0 rounded-xl border border-border bg-input overflow-hidden transition-colors focus-within:border-ring/40 focus-within:ring-2 focus-within:ring-ring/15 mb-3"
          >
            <PipelineYamlCodeEditor
              v-if="open"
              ref="codeEditorRef"
              v-model="yamlContent"
              :secret-names="secretNames"
              :variable-names="variableNames"
              class="h-full"
              @validated="onEditorValidated"
            />

            <!-- Re-open chip after manual dismiss (stays in editor corner) -->
            <button
              v-if="showFloatReopenChip"
              type="button"
              :class="
                cn(
                  'absolute top-3 right-3 z-20 inline-flex items-center gap-1.5 rounded-lg bg-background/95 px-2.5 py-1.5 text-xs font-medium shadow-md backdrop-blur-sm',
                  floatWarningsOnly
                    ? 'border border-amber-500/35 text-amber-700 dark:text-amber-400 hover:bg-amber-500/5'
                    : 'border border-destructive/30 text-destructive hover:bg-destructive/5',
                )
              "
              title="Show orchestration issues"
              @click="reopenFloat"
            >
              <AlertTriangle v-if="floatWarningsOnly" class="h-3.5 w-3.5" />
              <AlertCircle v-else class="h-3.5 w-3.5" />
              <template v-if="floatWarningsOnly">
                {{ floatWarnings.length }} warning{{ floatWarnings.length === 1 ? '' : 's' }}
              </template>
              <template v-else>
                {{ floatErrors.length }} error{{ floatErrors.length === 1 ? '' : 's' }}
                <template v-if="floatWarnings.length">
                  · {{ floatWarnings.length }} warn
                </template>
              </template>
            </button>
          </div>

          <div
            v-if="yamlResult?.valid && !floatWarnings.length"
            class="flex items-center gap-1 shrink-0 mb-1 text-xs text-emerald-600 dark:text-emerald-400"
          >
            <CheckCircle class="h-3.5 w-3.5" />
            Valid
          </div>
          <div
            v-else-if="yamlResult?.valid && floatWarnings.length"
            class="flex items-center gap-1 shrink-0 mb-1 text-xs text-amber-600 dark:text-amber-400"
          >
            <AlertTriangle class="h-3.5 w-3.5" />
            Valid with {{ floatWarnings.length }} warning{{ floatWarnings.length === 1 ? '' : 's' }}
          </div>
          </div>

          <div v-show="dialogTab === 'reference'" class="shrink-0 pb-1 space-y-2">
            <p class="text-xs text-muted-foreground leading-relaxed px-0.5">
              Cheat sheet for the YAML editor. Prefer a pinned <code class="font-mono text-[11px]">uses:</code> plugin whenever it provides the needed operation; keep inline scripts for project-specific work. Durations:
              <code class="font-mono text-[11px]">30</code> /
              <code class="font-mono text-[11px]">30s</code> /
              <code class="font-mono text-[11px]">10m</code> /
              <code class="font-mono text-[11px]">1h</code>
              (no spaces). Secrets &amp; Variables must already exist under Infrastructure. Marketplace examples use pinned versions; make sure their spec files are in the Manager's configured local step index (the default is <code class="font-mono text-[11px]">&lt;yaml_scan_dir&gt;/steps</code>). Browse available templates from the Plugin Marketplace link above. A <code class="font-mono text-[11px]">uses:</code> step gets its engine, image, and any requested capabilities from the template — <code class="font-mono text-[11px]">engine:</code> / <code class="font-mono text-[11px]">image:</code> alongside <code class="font-mono text-[11px]">uses:</code> are rejected by the validator. <code class="font-mono text-[11px]">run:</code> is the extension point: on a <code class="font-mono text-[11px]">uses:</code> step it is appended after the template's run block (the template provides the environment, the call site provides extra commands). Step inputs go under <code class="font-mono text-[11px]">with:</code>; a step-level <code class="font-mono text-[11px]">work_dir:</code> (optional, workspace-relative) overrides the template's <code class="font-mono text-[11px]">default_dir</code>. Capability requests are locked plugin metadata, not pipeline grants; a Runner administrator must separately authorize them in that machine's local Runner config (<code class="font-mono text-[11px]">docker_socket_allowed = true</code>).
            </p>
<pre class="rounded-lg bg-muted p-3 text-xs font-mono text-muted-foreground overflow-x-auto">name: hello-world
description: Sample pipeline covering common fields
team: default                     # team slug; omit → default (alias: owner_group)

# ── Trigger (pick ONE of manual / webhook / cron) ──
on:
  manual:
    inputs:
      version:
        description: Version to deploy
        required: true
        type: string                # optional; string is the default
        pattern: "^[A-Za-z0-9._-]+$"  # regex — validated at trigger
      deploy_host:
        description: Host name from Infrastructure → Hosts
        required: true
        type: host                  # must match a registered Host name
      deploy_token:
        description: One-time deploy token
        required: true
        masking: true               # redact logs/audit; encrypted in job DB
      environment:
        description: Target environment
        type: choice                # dropdown in trigger dialog
        options: [staging, production]
      replicas:
        type: number                # must be an integer
        default: "3"
      dry_run:
        type: boolean               # true / false (case-insensitive)
        default: "false"

  # webhook:
  #   path: abc123def456

  # cron:
  #   schedule: "0 2 * * *"

# ── Runner ──
runner:
  labels: [linux-x64]             # optional; runner must match ALL labels (AND)
  timeout: 60m                    # whole-job limit: 60 / 60s / 60m / 1h

# ── Concurrency (default mode = discard-old when mode omitted) ──
concurrency:
  group: prod

# concurrency:
#   group: prod
#   mode: queue                   # FIFO wait
#   max_queue: 5
#   queue_timeout: 30m

# concurrency:
#   group: prod
#   mode: discard-new             # reject while a job is active

# ── Inline env (non-secret) ──
env:
  APP_NAME: myapp
  LOG_LEVEL: info

# ── Platform Variables (declare names that already exist in Manager) ──
variables:
  - BUILD_CACHE_DIR
  - NOTIFY_EMAIL

# ── Secrets (optional top-level list — GHA-style) ──
# Prefer referencing at the use site; listing a name does NOT inject $NAME.
# On save, Manager also collects names from ${secrets.X} in env: / with:.
# secrets:
#   - GIT_TOKEN
#   - REGISTRY_HOST
#   - REGISTRY_USERNAME
#   - REGISTRY_PASSWORD
#   - CONFIG_BUNDLE_URL
#   - SLACK_WEBHOOK
#   - POSTGRES_PASSWORD

steps:
  # Checkout into the job workspace so later toolchain plugins share the files.
  - name: Checkout
    uses: buz/git-clone@1.1.0
    with:
      repo: https://github.com/example/hello-world.git
      into: source                          # clone destination (default source); no .. components

  - name: Build
    uses: buz/setup-rust@1.1.0                # template supplies a pinned Docker toolchain
    work_dir: source                        # optional step-level override of the template's default_dir: source
    with:
      script: cargo build --release
    run: echo "IMAGE_TAG=1.0.0" >> "$BUZ_ENV"   # appended after the template's run block
    timeout: 10m
    retry: 2

  - name: Unit Tests
    uses: buz/setup-node@1.1.0
    work_dir: source/frontend               # step-level work_dir (template default_dir: source)
    with:
      script: |
        npm ci
        npm test
    timeout: 15m

  # Login on the runner BEFORE pulling / pushing private images.
  # with: registry, username, password are required; reference Secrets by name.
  - name: Registry Login
    uses: buz/docker-login@1.1.0
    with:
      registry: ${secrets.REGISTRY_HOST}         # crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com
      username: ${secrets.REGISTRY_USERNAME}
      password: ${secrets.REGISTRY_PASSWORD}
      # debug: "false"                         # optional; defaults to false

  - name: Run Private Image
    engine: docker
    image: crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com/ns/app:1.0
    run: echo "pulled private image"
    timeout: 10m

  # Build and tag through the Docker plugin; Runner-admin authorization is separate.
  # buz/docker@1.1.0 requests docker-socket; do not grant capabilities in pipeline YAML.
  # Only enable docker_socket_allowed on a dedicated Runner for trusted jobs.
  # Socket access gives root-equivalent control of that Runner's Docker host.
  - name: Docker Build
    uses: buz/docker@1.1.0
    work_dir: source                        # step-level; default_dir: source makes this optional
    with:
      context: .                                      # relative to the step's work_dir
      tags: myapp:${env.IMAGE_TAG}          # dynamic env from an earlier step's $BUZ_ENV (Runner-resolved)
      push: "false"                         # local tags only; not a published release
    timeout: 10m

  # Optional cleanup AFTER the last registry use (e.g. shared isolation mode).
  # Runner job-scoped credentials are cleaned up when the job ends.
  # - name: Registry Logout
  #   uses: buz/docker-logout@1.0.0
  #   with:
  #     registry: ${secrets.REGISTRY_HOST}
  # This example uses the login template's default shell engine.

  # Fetch a .tar.gz bundle, then render in a separate step in the job workspace.
  # with.dest is workspace-relative; use a clean path without .. components.
  - name: Fetch Config
    uses: buz/config-fetch@1.1.0
    with:
      url: ${secrets.CONFIG_BUNDLE_URL}       # full presigned .tar.gz URL, including filename
      dest: config                           # fetch destination; does NOT set $BUZ_CONFIG_DIR

  # Template under dest may use ${vars.*} / ${secrets.*}, e.g.:
  #   MANAGER_IMAGE=${vars.MANAGER_IMAGE_TAG:-latest}
  #   POSTGRES_PASSWORD=${secrets.POSTGRES_PASSWORD}
  # Match config.dest to the prior fetch destination; render paths are relative to it.
  - name: Render Env
    engine: shell
    secrets: [POSTGRES_PASSWORD]
    config:
      dest: config
      render:
        - template: env.example
          output: .env
          mode: "0600"
          on_missing: fail
    run: test -f "$BUZ_CONFIG_DIR/.env"          # project-specific post-render assertion

  # Independent SSH deployment example: the target must ALREADY contain the project
  # and its configuration. Local fetch/render above does not transfer files to it.
  # Requires Buz >= 1.7.5, Docker Compose, and an ALREADY PUBLISHED accessible image.
  # The template supplies the ssh engine; host selectors use $input_name, not $INPUT_*.
  - name: Deploy
    uses: buz/docker-compose@1.0.0
    host: $deploy_host
    with:
      project_dir: /opt/app
      image: nginx:stable

  # Private HTTPS checkout uses a separate Generic token Secret, not an SSH-key script.
  - name: Clone Private Repo
    uses: buz/git-clone@1.1.0
    with:
      repo: https://github.com/org/private-repo.git
      into: private-source                  # clone destination (default source)
      token: ${secrets.GIT_TOKEN}
    timeout: 5m

  # ── Plugin Marketplace examples (exact version pins) ──
  # Manager resolves uses: from its local step index, not directly from the website.
  # Add the plugin spec files from Plugin Marketplace to that index before saving.
  - name: Check service readiness
    uses: buz/http-check@1.1.0
    with:
      url: https://example.com/health

  - name: Notify Slack
    uses: buz/slack@1.0.0
    with:
      webhook: ${secrets.SLACK_WEBHOOK}

  - name: Upload build to OSS
    uses: buz/oss-upload@1.0.2
    with:
      source: dist/app.tar.gz
      bucket: my-release-bucket
      object: releases/app.tar.gz
      endpoint: https://oss-cn-hangzhou.aliyuncs.com
      region: cn-hangzhou
      access_key_id: ${secrets.OSS_ACCESS_KEY_ID}
      access_key_secret: ${secrets.OSS_ACCESS_KEY_SECRET}

# ────────────────────────────────────────────────────
# Resolution cheat sheet
#   host: / host_labels:     $input_name          (trigger input; not $INPUT_*)
#   run scripts:             $INPUT_*             (same inputs, uppercased)
#   pipeline env:            $APP_NAME
#   platform Variables:      $BUILD_CACHE_DIR
#   secrets in scripts:      only via env: KEY: ${secrets.NAME}  →  $KEY
#   dynamic env:             echo "KEY=value" >> "$BUZ_ENV"   (a later step or with: value reads ${env.KEY})
#   config.dest:             $BUZ_CONFIG_DIR
#   after config.render:     $BUZ_ENV_CHANGED*
#
# Secrets (top-level secrets: is optional)
#   env: KEY: ${secrets.NAME}       inject + allowlist
#   with: input: ${secrets.NAME}    plugin input + allowlist
#   with: input: ${env.KEY}         dynamic $BUZ_ENV value — NOT a secret ref;
#                                   masking inputs still require exactly ${secrets.NAME}
#   secrets: [NAME]                overview / step narrowing
#   never put ${secrets.…} inside run: scripts
#
# Config delivery: buz/config-fetch with a full bundle URL and explicit dest.
# Render-only config: match config.dest to the fetch destination.
# ────────────────────────────────────────────────────</pre>
          </div>
        </div>

        <!-- Footer — same alignment as Secrets dialog -->
        <div class="shrink-0 flex items-center justify-end gap-3 px-5 sm:px-6 py-4 border-t border-border/30">
          <UiButton variant="ghost" size="sm" :disabled="saving" @click="close">Cancel</UiButton>
          <template v-if="dialogTab === 'editor'">
            <UiButton
              size="sm"
              variant="outline"
              :disabled="yamlValidating || !yamlContent.trim()"
              @click="handleValidate"
            >
              <CheckCircle class="h-3.5 w-3.5" />
              {{ yamlValidating ? 'Validating...' : 'Validate' }}
            </UiButton>
            <UiButton
              size="sm"
              :disabled="
                saving ||
                yamlValidating ||
                !yamlContent.trim() ||
                !!filenameConflict ||
                (!!yamlResult && !yamlResult.valid)
              "
              @click="saveFromYaml"
            >
              <Loader2 v-if="saving" class="h-3.5 w-3.5 animate-spin" />
              {{ editingId ? 'Save Pipeline' : 'Create Pipeline' }}
            </UiButton>
          </template>
        </div>
      </div>

      <!-- Fixed to viewport; drag header to move anywhere on screen -->
      <Transition
        enter-active-class="transition duration-150 ease-out"
        enter-from-class="opacity-0"
        enter-to-class="opacity-100"
        leave-active-class="transition duration-100 ease-in"
        leave-from-class="opacity-100"
        leave-to-class="opacity-0"
      >
        <div
          v-if="floatVisible && hasFloatIssues && floatPos"
          ref="floatPanelRef"
          :class="
            cn(
              'fixed z-[60] flex w-[min(22rem,calc(100vw-1.5rem))] max-h-[min(70vh,24rem)] flex-col overflow-hidden rounded-xl bg-background/95 shadow-xl backdrop-blur-sm',
              floatWarningsOnly ? 'border border-amber-500/40' : 'border border-destructive/35',
            )
          "
          role="alert"
          :style="{ left: `${floatPos.left}px`, top: `${floatPos.top}px` }"
        >
          <div
            :class="
              cn(
                'flex items-center gap-1.5 px-2 py-2 shrink-0 select-none border-b',
                floatDragging ? 'cursor-grabbing' : 'cursor-grab',
                floatWarningsOnly
                  ? 'border-amber-500/25 bg-amber-500/5'
                  : 'border-destructive/20 bg-destructive/5',
              )
            "
            title="Drag to move"
            @pointerdown="onFloatHeaderPointerDown"
          >
            <GripVertical
              :class="
                cn(
                  'h-4 w-4 shrink-0 pointer-events-none',
                  floatWarningsOnly ? 'text-amber-500/60' : 'text-destructive/50',
                )
              "
            />
            <AlertTriangle
              v-if="floatWarningsOnly"
              class="h-4 w-4 text-amber-600 dark:text-amber-400 shrink-0 pointer-events-none"
            />
            <AlertCircle
              v-else
              class="h-4 w-4 text-destructive shrink-0 pointer-events-none"
            />
            <span
              :class="
                cn(
                  'text-sm font-medium flex-1 truncate pointer-events-none',
                  floatWarningsOnly
                    ? 'text-amber-700 dark:text-amber-400'
                    : 'text-destructive',
                )
              "
            >
              Orchestration issues
              <span
                :class="
                  cn(
                    'font-mono text-xs font-normal',
                    floatWarningsOnly
                      ? 'text-amber-700/80 dark:text-amber-400/80'
                      : 'text-destructive/80',
                  )
                "
              >
                <template v-if="floatWarningsOnly">
                  ({{ floatWarnings.length }} warning{{ floatWarnings.length === 1 ? '' : 's' }})
                </template>
                <template v-else>
                  ({{ floatErrors.length }}
                  <template v-if="floatWarnings.length">
                    · {{ floatWarnings.length }} warn
                  </template>)
                </template>
              </span>
            </span>
            <button
              type="button"
              data-float-dismiss
              class="inline-flex h-7 w-7 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground cursor-pointer"
              title="Dismiss"
              aria-label="Dismiss error panel"
              @click="dismissFloat"
            >
              <X class="h-3.5 w-3.5" />
            </button>
          </div>
          <ul class="min-h-0 flex-1 overflow-y-auto px-3 py-2 space-y-2.5">
            <li
              v-for="(e, i) in floatErrors"
              :key="'fe'+i"
              class="flex items-start gap-2 text-xs leading-relaxed cursor-pointer rounded-md px-1 -mx-1 py-1 transition-colors hover:bg-destructive/10"
              title="Click to jump to this line"
              @click="jumpToIssue(e.field, e.message)"
            >
              <span
                :class="
                  cn(
                    'mt-0.5 shrink-0 inline-flex items-center rounded px-1.5 py-0.5 text-[10px] font-bold tracking-wide uppercase ring-1 ring-inset',
                    severityBadgeClass('error'),
                  )
                "
              >error</span>
              <div class="min-w-0 flex-1">
                <span
                  v-if="e.field && e.field !== 'yaml' && e.field !== 'yaml.keys'"
                  class="font-mono font-semibold text-destructive bg-destructive/15 rounded px-1 py-px"
                >{{ e.field }}</span>
                <span
                  v-if="e.field && e.field !== 'yaml' && e.field !== 'yaml.keys'"
                  class="text-muted-foreground"
                > — </span>
                <span
                  v-for="(seg, si) in splitDiagnosticSegments(e.message)"
                  :key="'fes'+i+'-'+si"
                  :class="severityWordClass(seg.kind)"
                >{{ seg.text }}</span>
              </div>
            </li>
            <li
              v-for="(w, i) in floatWarnings"
              :key="'fw'+i"
              class="flex items-start gap-2 text-xs leading-relaxed cursor-pointer rounded-md px-1 -mx-1 py-1 transition-colors hover:bg-amber-500/10"
              title="Click to jump to this line"
              @click="jumpToIssue(splitWarningField(w).field, splitWarningField(w).message)"
            >
              <span
                :class="
                  cn(
                    'mt-0.5 shrink-0 inline-flex items-center rounded px-1.5 py-0.5 text-[10px] font-bold tracking-wide uppercase ring-1 ring-inset',
                    severityBadgeClass('warning'),
                  )
                "
              >warning</span>
              <div class="min-w-0 flex-1 text-muted-foreground">
                <span
                  v-for="(seg, si) in splitDiagnosticSegments(w)"
                  :key="'fws'+i+'-'+si"
                  :class="severityWordClass(seg.kind)"
                >{{ seg.text }}</span>
              </div>
            </li>
          </ul>
        </div>
      </Transition>
    </div>
  </Teleport>
</template>
