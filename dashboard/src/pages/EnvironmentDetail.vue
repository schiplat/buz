<script setup lang="ts">
import { ref, computed, onMounted, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import UiButton from '@/components/ui/UiButton.vue'
import TablePagination from '@/components/ui/TablePagination.vue'
import { Plus, Pencil, Trash2, ArrowLeft, X } from '@lucide/vue'
import {
  environmentsApi,
  type EnvironmentDetail,
  type EnvironmentOverride,
  type EnvironmentSecretOverride,
  type HostAllowlistEntry,
  type HostAllowlistKind,
} from '@/api/environments'
import { variablesApi, type Variable } from '@/api/variables'
import { secretsApi, type Secret } from '@/api/secrets'
import { useAuthStore } from '@/stores/auth'
import { formatLocalDateTime } from '@/utils/time'
import { useAppDialog } from '@/composables/useAppDialog'
import { pageTitle } from '@/utils/documentTitle'
import { useClientPagination } from '@/composables/useClientPagination'
import { useClientSort } from '@/composables/useClientSort'

const auth = useAuthStore()
const dialog = useAppDialog()
const route = useRoute()
const router = useRouter()

const envId = computed(() => route.params.id as string)

const env = ref<EnvironmentDetail | null>(null)
const allVars = ref<Variable[]>([])
const allSecrets = ref<Secret[]>([])
const loading = ref(true)
const error = ref('')

const overrides = computed(() => env.value?.overrides || [])
const secretOverrides = computed(() => env.value?.secret_overrides || [])

const {
  sorted: sortedOverrides,
  toggleSort: toggleVarSort,
  sortIndicator: varSortIndicator,
} = useClientSort(overrides, {
  initialKey: 'var_name',
  initialDir: 'asc',
  getValue: (row, key) => {
    switch (key) {
      case 'var_name': return row.var_name
      case 'default_value': return row.default_value
      case 'value': return row.value
      default: return ''
    }
  },
})

const {
  sorted: sortedSecretOverrides,
  toggleSort: toggleSecretSort,
  sortIndicator: secretSortIndicator,
} = useClientSort(secretOverrides, {
  initialKey: 'secret_name',
  initialDir: 'asc',
  getValue: (row, key) => {
    switch (key) {
      case 'secret_name': return row.secret_name
      case 'kind': return row.kind
      case 'status': return row.has_override ? 'custom' : ''
      default: return ''
    }
  },
})

const {
  page: varPage,
  pageSize: varPageSize,
  pageCount: varPageCount,
  total: varTotal,
  pageItems: pagedOverrides,
  rangeLabel: varRangeLabel,
} = useClientPagination(sortedOverrides, { initialPageSize: 10 })

const {
  page: secretPage,
  pageSize: secretPageSize,
  pageCount: secretPageCount,
  total: secretTotal,
  pageItems: pagedSecretOverrides,
  rangeLabel: secretRangeLabel,
} = useClientPagination(sortedSecretOverrides, { initialPageSize: 10 })

const unboundVars = computed(() => {
  const taken = new Set(overrides.value.map((o) => o.var_id))
  return allVars.value.filter((v) => !taken.has(v.id))
})

const unboundSecrets = computed(() => {
  const taken = new Set(secretOverrides.value.map((o) => o.secret_id))
  return allSecrets.value.filter((s) => !taken.has(s.id))
})

const editingEnv = ref(false)
const formName = ref('')
const formSlug = ref('')
const formDescription = ref('')
const formProtectionEnabled = ref(false)
const formMinTriggerRole = ref('developer')
const formMinApproveRole = ref('maintainer')
const formRequireApprovals = ref(1)
const formForbidSelfApprove = ref(true)
const formApprovalTimeoutSeconds = ref(86400)
const formDeploymentConcurrency = ref<'off' | 'queue' | 'cancel_in_progress'>('off')
const savingEnv = ref(false)
const allowlistKind = ref<HostAllowlistKind>('name')
const allowlistValue = ref('')
const savingAllowlist = ref(false)
const allowlistError = ref('')

const ROLE_OPTIONS = ['developer', 'maintainer', 'owner'] as const

const showOverrideDialog = ref(false)
const overrideVarId = ref('')
const overrideValue = ref('')
const savingOverride = ref(false)
const overrideDialogError = ref('')

const showSecretOverrideDialog = ref(false)
const overrideSecretId = ref('')
const overrideSecretValue = ref('')
const savingSecretOverride = ref(false)
const secretOverrideDialogError = ref('')

async function loadDetail() {
  loading.value = true
  error.value = ''
  try {
    const [detail, vars, secrets] = await Promise.all([
      environmentsApi.get(envId.value),
      auth.isPlatformAdmin ? variablesApi.list() : Promise.resolve([] as Variable[]),
      auth.isPlatformAdmin ? secretsApi.list() : Promise.resolve([] as Secret[]),
    ])
    env.value = detail
    allVars.value = vars
    allSecrets.value = secrets
    document.title = pageTitle(detail.name)
    if (editingEnv.value) syncFormFromEnv()
  } catch (e: any) {
    env.value = null
    error.value = e?.response?.data?.message || e?.message || 'Failed to load environment'
  } finally {
    loading.value = false
  }
}

function syncFormFromEnv() {
  if (!env.value) return
  formName.value = env.value.name
  formSlug.value = env.value.slug
  formDescription.value = env.value.description || ''
  formProtectionEnabled.value = !!env.value.protection_enabled
  formMinTriggerRole.value = (env.value.min_trigger_role as string) || 'developer'
  formMinApproveRole.value = (env.value.min_approve_role as string) || 'maintainer'
  formRequireApprovals.value = Math.max(1, env.value.require_approvals ?? 1)
  formForbidSelfApprove.value = env.value.forbid_self_approve !== false
  formApprovalTimeoutSeconds.value = env.value.approval_timeout_seconds ?? 86400
  formDeploymentConcurrency.value = env.value.deployment_concurrency || 'off'
}

function startEditEnv() {
  syncFormFromEnv()
  editingEnv.value = true
}

function cancelEditEnv() {
  editingEnv.value = false
  error.value = ''
}

async function saveEnv() {
  if (!env.value || !formName.value.trim() || !formSlug.value.trim()) {
    error.value = 'Name and slug are required'
    return
  }
  savingEnv.value = true
  error.value = ''
  try {
    const protection = auth.isPlatformAdmin
      ? {
          protection_enabled: formProtectionEnabled.value,
          min_trigger_role: formMinTriggerRole.value,
          min_approve_role: formMinApproveRole.value,
          require_approvals: Math.max(1, Number(formRequireApprovals.value) || 1),
          forbid_self_approve: formForbidSelfApprove.value,
          approval_timeout_seconds: Math.max(60, Number(formApprovalTimeoutSeconds.value) || 86400),
          deployment_concurrency: formDeploymentConcurrency.value,
        }
      : {}
    await environmentsApi.update(env.value.id, {
      name: formName.value.trim(),
      slug: formSlug.value.trim(),
      description: formDescription.value,
      ...protection,
    })
    editingEnv.value = false
    await loadDetail()
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to save environment'
  } finally {
    savingEnv.value = false
  }
}

async function addHostAllowlistEntry() {
  if (!env.value || !allowlistValue.value.trim()) {
    allowlistError.value = 'Enter a host name or label'
    return
  }
  savingAllowlist.value = true
  allowlistError.value = ''
  try {
    await environmentsApi.addHostAllowlist(env.value.id, {
      kind: allowlistKind.value,
      value: allowlistValue.value.trim(),
    })
    allowlistValue.value = ''
    await loadDetail()
  } catch (e: any) {
    allowlistError.value = e?.response?.data?.message || e?.message || 'Failed to add allowlist entry'
  } finally {
    savingAllowlist.value = false
  }
}

async function removeHostAllowlistEntry(entry: HostAllowlistEntry) {
  if (!env.value) return
  if (!(await dialog.confirm(`Remove ${entry.kind} “${entry.value}” from this environment's host allowlist?`, {
    title: 'Remove host allowlist entry',
    confirmLabel: 'Remove',
    danger: true,
  }))) return
  allowlistError.value = ''
  try {
    await environmentsApi.deleteHostAllowlist(env.value.id, entry)
    await loadDetail()
  } catch (e: any) {
    allowlistError.value = e?.response?.data?.message || e?.message || 'Failed to remove allowlist entry'
  }
}

async function deleteEnv() {
  if (!env.value) return
  if (!(await dialog.confirm(
    `Delete environment "${env.value.name}"? Overrides are removed; jobs keep history with a cleared link.`,
    { title: 'Delete environment', confirmLabel: 'Delete', danger: true },
  ))) return
  error.value = ''
  try {
    await environmentsApi.delete(env.value.id)
    void router.push({ name: 'environments' })
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to delete'
  }
}

function openAddOverride() {
  overrideVarId.value = unboundVars.value[0]?.id || ''
  overrideValue.value = unboundVars.value[0]?.value || ''
  overrideDialogError.value = ''
  showOverrideDialog.value = true
}

watch(overrideVarId, (id) => {
  const v = allVars.value.find((x) => x.id === id)
  if (v && showOverrideDialog.value) overrideValue.value = v.value
})

async function saveOverride() {
  if (!env.value || !overrideVarId.value) return
  savingOverride.value = true
  overrideDialogError.value = ''
  try {
    await environmentsApi.upsertOverride(env.value.id, {
      var_id: overrideVarId.value,
      value: overrideValue.value,
    })
    showOverrideDialog.value = false
    await loadDetail()
  } catch (e: any) {
    overrideDialogError.value = e?.response?.data?.message || e?.message || 'Failed to save override'
  } finally {
    savingOverride.value = false
  }
}

async function clearOverride(o: EnvironmentOverride) {
  if (!env.value) return
  if (!(await dialog.confirm(
    `Clear override for ${o.var_name}? This environment will inherit the shared Default again.`,
    { title: 'Clear override', confirmLabel: 'Clear', danger: true },
  ))) return
  error.value = ''
  try {
    await environmentsApi.deleteOverride(env.value.id, o.var_id)
    await loadDetail()
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to clear override'
  }
}

function openAddSecretOverride() {
  overrideSecretId.value = unboundSecrets.value[0]?.id || ''
  overrideSecretValue.value = ''
  secretOverrideDialogError.value = ''
  showSecretOverrideDialog.value = true
}

async function saveSecretOverride() {
  if (!env.value || !overrideSecretId.value || !overrideSecretValue.value.trim()) {
    secretOverrideDialogError.value = 'Secret and override value are required'
    return
  }
  savingSecretOverride.value = true
  secretOverrideDialogError.value = ''
  try {
    await environmentsApi.upsertSecretOverride(env.value.id, {
      secret_id: overrideSecretId.value,
      value: overrideSecretValue.value,
    })
    showSecretOverrideDialog.value = false
    overrideSecretValue.value = ''
    await loadDetail()
  } catch (e: any) {
    secretOverrideDialogError.value = e?.response?.data?.message || e?.message || 'Failed to save secret override'
  } finally {
    savingSecretOverride.value = false
  }
}

async function clearSecretOverride(o: EnvironmentSecretOverride) {
  if (!env.value) return
  if (!(await dialog.confirm(
    `Clear secret override for ${o.secret_name}? This environment will inherit the shared Default again.`,
    { title: 'Clear secret override', confirmLabel: 'Clear', danger: true },
  ))) return
  error.value = ''
  try {
    await environmentsApi.deleteSecretOverride(env.value.id, o.secret_id)
    await loadDetail()
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to clear secret override'
  }
}

watch(envId, () => {
  editingEnv.value = false
  void loadDetail()
})

onMounted(() => {
  void loadDetail()
})
</script>

<template>
  <div class="flex flex-col gap-5">
    <!-- Top bar -->
    <div class="flex items-start justify-between gap-3">
      <div class="flex items-start gap-2.5 min-w-0">
        <button
          class="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground transition-colors shrink-0 mt-1"
          @click="router.push('/environments')"
        >
          <ArrowLeft class="h-3.5 w-3.5" />
        </button>
        <div class="h-5 w-px bg-border/40 shrink-0 mt-1.5" />
        <div class="min-w-0">
          <div class="flex items-center gap-2 min-w-0">
            <h1 class="text-base font-semibold tracking-tight truncate">{{ env?.name || 'Loading…' }}</h1>
            <span
              v-if="env?.protection_enabled"
              class="inline-flex items-center rounded-full border border-amber-500/30 bg-amber-500/10 px-2 py-0.5 text-[11px] font-medium text-amber-700 dark:text-amber-400 shrink-0"
            >Protected</span>
          </div>
          <p v-if="env" class="mt-0.5 text-[12px] text-muted-foreground truncate">
            <code class="font-mono text-[11px] text-muted-foreground/80">{{ env.slug }}</code>
            <template v-if="env.description">
              <span class="mx-1.5 text-muted-foreground/40">·</span>
              <span :title="env.description">{{ env.description }}</span>
            </template>
          </p>
        </div>
      </div>
      <div v-if="auth.isPlatformAdmin && env && !editingEnv" class="flex items-center gap-2 shrink-0 mt-0.5">
        <UiButton size="sm" variant="outline" @click="startEditEnv">
          <Pencil class="h-3.5 w-3.5" /> Edit
        </UiButton>
        <UiButton size="sm" variant="ghost" title="Delete" @click="deleteEnv">
          <Trash2 class="h-3.5 w-3.5 text-red-500" />
        </UiButton>
      </div>
    </div>

    <p v-if="error" class="rounded-lg bg-red-500/10 border border-red-500/20 px-3 py-2 text-[12px] text-red-700 dark:text-red-400">
      {{ error }}
    </p>

    <div v-if="loading" class="text-center py-12 text-muted-foreground text-sm">Loading…</div>

    <template v-else-if="env">
      <!-- Environment info (editable) -->
      <div class="rounded-xl border border-border/50 bg-card shadow-sm overflow-hidden">
        <template v-if="!editingEnv">
          <div class="px-5 py-4 space-y-4">
            <div>
              <p class="text-[11px] font-medium text-muted-foreground uppercase tracking-wider mb-2">Protection</p>
              <template v-if="env.protection_enabled">
                <div class="flex flex-wrap gap-x-6 gap-y-3 text-[12px]">
                  <div>
                    <span class="text-muted-foreground">Min trigger role</span>
                    <div class="mt-0.5 font-medium">{{ env.min_trigger_role || 'developer' }}</div>
                  </div>
                  <div>
                    <span class="text-muted-foreground">Min approve role</span>
                    <div class="mt-0.5 font-medium">{{ env.min_approve_role || 'maintainer' }}</div>
                  </div>
                  <div>
                    <span class="text-muted-foreground">Required approvals</span>
                    <div class="mt-0.5 font-medium tabular-nums">{{ env.require_approvals ?? 1 }}</div>
                  </div>
                  <div>
                    <span class="text-muted-foreground">Self-approve</span>
                    <div class="mt-0.5 font-medium">{{ env.forbid_self_approve !== false ? 'Forbidden' : 'Allowed' }}</div>
                  </div>
                  <div>
                    <span class="text-muted-foreground">Timeout</span>
                    <div class="mt-0.5 font-medium tabular-nums">{{ env.approval_timeout_seconds ?? 86400 }}s</div>
                  </div>
                </div>
              </template>
              <p v-else class="text-xs text-muted-foreground">
                Not protected — jobs targeting this environment start without approval.
              </p>
            </div>
            <div class="flex flex-wrap gap-x-6 gap-y-3 text-[12px]">
              <div>
                <span class="text-muted-foreground">Deployment concurrency</span>
                <div class="mt-0.5 font-medium font-mono">{{ env.deployment_concurrency || 'off' }}</div>
              </div>
              <div>
                <span class="text-muted-foreground">Host allowlist</span>
                <div class="mt-0.5 font-medium">{{ env.host_allowlist.length ? `${env.host_allowlist.length} entries` : 'Unrestricted' }}</div>
              </div>
              <div v-if="env.updated_at">
                <span class="text-muted-foreground">Updated</span>
                <div class="mt-0.5 tabular-nums text-foreground/90">{{ formatLocalDateTime(env.updated_at) }}</div>
              </div>
            </div>
            <p class="text-[11px] text-muted-foreground">
              An empty host allowlist permits any target. With entries present, only listed Host names or labels are allowed; dynamic SSH targets are rejected.
            </p>
          </div>
        </template>

        <template v-else>
          <div class="px-5 py-4 space-y-4">
            <div class="flex items-center justify-between">
              <p class="text-[13px] font-medium">Edit environment</p>
              <button
                type="button"
                class="text-muted-foreground hover:text-foreground transition-colors"
                :disabled="savingEnv"
                @click="cancelEditEnv"
              >
                <X class="h-4 w-4" />
              </button>
            </div>
            <div class="grid grid-cols-1 sm:grid-cols-2 gap-3">
              <div>
                <label class="block text-[12px] font-medium mb-1">Name <span class="text-red-500">*</span></label>
                <input v-model="formName" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50" placeholder="Production" />
              </div>
              <div>
                <label class="block text-[12px] font-medium mb-1">Slug <span class="text-red-500">*</span></label>
                <input v-model="formSlug" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50" placeholder="production" />
              </div>
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Description</label>
              <input v-model="formDescription" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50" placeholder="Optional" />
            </div>

            <div class="rounded-lg border border-border/40 bg-muted/20 p-3 space-y-3">
              <label class="flex items-start gap-2 cursor-pointer">
                <input v-model="formProtectionEnabled" type="checkbox" class="mt-0.5" />
                <span class="text-[12px] leading-snug">
                  <span class="font-medium">Enable protection</span>
                  <span class="block text-[11px] text-muted-foreground mt-0.5">
                    Jobs using this environment wait for approval before running.
                  </span>
                </span>
              </label>
              <div>
                <label class="block text-[12px] font-medium mb-1">Deployment concurrency</label>
                <select v-model="formDeploymentConcurrency" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50">
                  <option value="off">off — no cross-pipeline serialization</option>
                  <option value="queue">queue — FIFO across pipelines</option>
                  <option value="cancel_in_progress">cancel_in_progress — replace active deployment</option>
                </select>
              </div>
              <template v-if="formProtectionEnabled">
                <div class="grid grid-cols-2 gap-3">
                  <div>
                    <label class="block text-[12px] font-medium mb-1">Min trigger role <span class="text-red-500">*</span></label>
                    <select v-model="formMinTriggerRole" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50">
                      <option v-for="r in ROLE_OPTIONS" :key="r" :value="r">{{ r }}</option>
                    </select>
                  </div>
                  <div>
                    <label class="block text-[12px] font-medium mb-1">Min approve role <span class="text-red-500">*</span></label>
                    <select v-model="formMinApproveRole" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50">
                      <option v-for="r in ROLE_OPTIONS" :key="r" :value="r">{{ r }}</option>
                    </select>
                  </div>
                </div>
                <div class="grid grid-cols-2 gap-3">
                  <div>
                    <label class="block text-[12px] font-medium mb-1">Require approvals <span class="text-red-500">*</span></label>
                    <input
                      v-model.number="formRequireApprovals"
                      type="number"
                      min="1"
                      class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50"
                    />
                  </div>
                  <div>
                    <label class="block text-[12px] font-medium mb-1">Timeout (seconds) <span class="text-red-500">*</span></label>
                    <input
                      v-model.number="formApprovalTimeoutSeconds"
                      type="number"
                      min="60"
                      class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50"
                    />
                  </div>
                </div>
                <label class="flex items-start gap-2 cursor-pointer">
                  <input v-model="formForbidSelfApprove" type="checkbox" class="mt-0.5" />
                  <span class="text-[12px] leading-snug">
                    <span class="font-medium">Forbid self-approve</span>
                    <span class="block text-[11px] text-muted-foreground mt-0.5">
                      The user who triggered the job cannot approve it.
                    </span>
                  </span>
                </label>
              </template>
            </div>

            <div class="flex justify-end gap-3 pt-2 border-t border-border/30">
              <UiButton variant="ghost" size="sm" :disabled="savingEnv" @click="cancelEditEnv">Cancel</UiButton>
              <UiButton size="sm" :disabled="savingEnv" @click="saveEnv">{{ savingEnv ? 'Saving…' : 'Save' }}</UiButton>
            </div>
          </div>
        </template>
      </div>

      <div class="rounded-xl border border-border/50 bg-card shadow-sm overflow-hidden">
        <div class="flex items-center justify-between gap-3 px-5 py-3 border-b border-border/30">
          <div>
            <p class="text-[13px] font-medium">Host name / label allowlist</p>
            <p class="mt-0.5 text-[11px] text-muted-foreground">
              Empty means unrestricted. Once populated, each static SSH target must match a listed Host name or label.
            </p>
          </div>
        </div>
        <div class="p-5 space-y-3">
          <p v-if="allowlistError" class="rounded-lg bg-red-500/10 border border-red-500/20 px-3 py-2 text-[12px] text-red-700 dark:text-red-400">
            {{ allowlistError }}
          </p>
          <form v-if="auth.isPlatformAdmin" class="flex flex-col sm:flex-row gap-2" @submit.prevent="addHostAllowlistEntry">
            <select v-model="allowlistKind" class="rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50">
              <option value="name">Host name</option>
              <option value="label">Host label</option>
            </select>
            <input v-model="allowlistValue" class="flex-1 rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50" :placeholder="allowlistKind === 'name' ? 'prod-01' : 'production'" />
            <UiButton type="submit" size="sm" :disabled="savingAllowlist || !allowlistValue.trim()"><Plus class="h-3.5 w-3.5" /> Add</UiButton>
          </form>
          <div v-if="env.host_allowlist.length" class="divide-y divide-border/20 rounded-lg border border-border/30">
            <div v-for="entry in env.host_allowlist" :key="`${entry.kind}:${entry.value}`" class="flex items-center gap-3 px-3 py-2.5">
              <span class="mono-chip text-[10px] uppercase">{{ entry.kind }}</span>
              <code class="flex-1 break-all text-xs">{{ entry.value }}</code>
              <UiButton v-if="auth.isPlatformAdmin" variant="ghost" size="sm" title="Remove allowlist entry" @click="removeHostAllowlistEntry(entry)"><Trash2 class="h-3.5 w-3.5 text-red-500" /></UiButton>
            </div>
          </div>
          <p v-else class="py-3 text-center text-xs text-muted-foreground">
            No restrictions: all registered Host names and labels may be targeted, including dynamic SSH targets.
          </p>
        </div>
      </div>

      <!-- Variable | Secret overrides (left / right) -->
      <div class="grid grid-cols-1 xl:grid-cols-2 gap-5">
        <div class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm flex flex-col min-w-0">
          <div class="flex items-center justify-between gap-3 px-5 py-3 border-b border-border/30 shrink-0">
            <div class="min-w-0">
              <p class="text-[13px] font-medium">Variable overrides</p>
              <p class="text-[11px] text-muted-foreground mt-0.5 truncate">
                Unbound Variables keep the shared Default.
              </p>
            </div>
            <UiButton
              v-if="auth.isPlatformAdmin"
              size="sm"
              class="shrink-0"
              :disabled="unboundVars.length === 0"
              @click="openAddOverride"
            >
              <Plus class="h-3.5 w-3.5" /> Add
            </UiButton>
          </div>
          <div v-if="overrides.length === 0" class="px-5 py-12 text-center text-sm text-muted-foreground">
            No Variable overrides
          </div>
          <template v-else>
            <div class="overflow-x-auto">
              <table class="w-full text-[13px]">
                <thead>
                  <tr class="border-b border-border/30 bg-muted/10">
                    <th class="text-left px-4 py-2.5">
                      <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleVarSort('var_name')">
                        Variable
                        <span class="font-mono text-xs">{{ varSortIndicator('var_name') }}</span>
                      </button>
                    </th>
                    <th class="text-left px-4 py-2.5">
                      <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleVarSort('default_value')">
                        Default
                        <span class="font-mono text-xs">{{ varSortIndicator('default_value') }}</span>
                      </button>
                    </th>
                    <th class="text-left px-4 py-2.5">
                      <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleVarSort('value')">
                        Override
                        <span class="font-mono text-xs">{{ varSortIndicator('value') }}</span>
                      </button>
                    </th>
                    <th class="text-right px-4 py-2.5 w-12" />
                  </tr>
                </thead>
                <tbody class="divide-y divide-border/20">
                  <tr v-for="o in pagedOverrides" :key="o.var_id" class="hover:bg-muted/5">
                    <td class="px-4 py-3">
                      <span
                        class="block font-mono text-[12px] font-medium leading-snug text-foreground/90 break-all"
                        :title="o.var_name"
                      >{{ o.var_name }}</span>
                    </td>
                    <td class="px-4 py-3"><code class="mono-chip text-xs break-all">{{ o.default_value }}</code></td>
                    <td class="px-4 py-3"><code class="mono-chip text-xs break-all">{{ o.value }}</code></td>
                    <td class="px-4 py-3 text-right">
                      <UiButton
                        v-if="auth.isPlatformAdmin"
                        variant="ghost"
                        size="sm"
                        title="Clear → Inherit"
                        @click="clearOverride(o)"
                      >
                        <Trash2 class="h-3.5 w-3.5 text-red-500" />
                      </UiButton>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
            <TablePagination
              v-model:page="varPage"
              v-model:pageSize="varPageSize"
              :total="varTotal"
              :page-count="varPageCount"
              :range-label="varRangeLabel"
            />
          </template>
        </div>

        <div class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm flex flex-col min-w-0">
          <div class="flex items-center justify-between gap-3 px-5 py-3 border-b border-border/30 shrink-0">
            <div class="min-w-0">
              <p class="text-[13px] font-medium">Secret overrides</p>
              <p class="text-[11px] text-muted-foreground mt-0.5 truncate">
                Values never shown after save; allowlist only.
              </p>
            </div>
            <UiButton
              v-if="auth.isPlatformAdmin"
              size="sm"
              class="shrink-0"
              :disabled="unboundSecrets.length === 0"
              @click="openAddSecretOverride"
            >
              <Plus class="h-3.5 w-3.5" /> Add
            </UiButton>
          </div>
          <div v-if="secretOverrides.length === 0" class="px-5 py-12 text-center text-sm text-muted-foreground">
            No Secret overrides
          </div>
          <template v-else>
            <div class="overflow-x-auto">
              <table class="w-full text-[13px]">
                <thead>
                  <tr class="border-b border-border/30 bg-muted/10">
                    <th class="text-left px-4 py-2.5">
                      <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSecretSort('secret_name')">
                        Secret
                        <span class="font-mono text-xs">{{ secretSortIndicator('secret_name') }}</span>
                      </button>
                    </th>
                    <th class="text-left px-4 py-2.5">
                      <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSecretSort('kind')">
                        Kind
                        <span class="font-mono text-xs">{{ secretSortIndicator('kind') }}</span>
                      </button>
                    </th>
                    <th class="text-left px-4 py-2.5">
                      <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSecretSort('status')">
                        Status
                        <span class="font-mono text-xs">{{ secretSortIndicator('status') }}</span>
                      </button>
                    </th>
                    <th class="text-right px-4 py-2.5 w-12" />
                  </tr>
                </thead>
                <tbody class="divide-y divide-border/20">
                  <tr v-for="o in pagedSecretOverrides" :key="o.secret_id" class="hover:bg-muted/5">
                    <td class="px-4 py-3">
                      <span
                        class="block font-mono text-[12px] font-medium leading-snug text-foreground/90 break-all"
                        :title="o.secret_name"
                      >{{ o.secret_name }}</span>
                    </td>
                    <td class="px-4 py-3 text-xs text-muted-foreground">{{ o.kind }}</td>
                    <td class="px-4 py-3">
                      <span class="mono-chip text-xs">Custom</span>
                    </td>
                    <td class="px-4 py-3 text-right">
                      <UiButton
                        v-if="auth.isPlatformAdmin"
                        variant="ghost"
                        size="sm"
                        title="Clear → Inherit"
                        @click="clearSecretOverride(o)"
                      >
                        <Trash2 class="h-3.5 w-3.5 text-red-500" />
                      </UiButton>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
            <TablePagination
              v-model:page="secretPage"
              v-model:pageSize="secretPageSize"
              :total="secretTotal"
              :page-count="secretPageCount"
              :range-label="secretRangeLabel"
            />
          </template>
        </div>
      </div>
    </template>

    <!-- Add Variable override -->
    <Teleport to="body">
      <div v-if="showOverrideDialog" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="showOverrideDialog = false" />
        <div class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6">
          <h3 class="text-[15px] font-semibold mb-5">Add Variable override</h3>
          <p
            v-if="overrideDialogError"
            class="mb-4 rounded-lg bg-red-500/10 border border-red-500/20 px-3 py-2 text-[12px] text-red-700 dark:text-red-400"
          >
            {{ overrideDialogError }}
          </p>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium mb-1">Variable <span class="text-red-500">*</span></label>
              <select v-model="overrideVarId" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50">
                <option v-for="v in unboundVars" :key="v.id" :value="v.id">{{ v.name }}</option>
              </select>
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Override value <span class="text-red-500">*</span></label>
              <textarea v-model="overrideValue" rows="4" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50 resize-y" />
            </div>
          </div>
          <div class="flex justify-end gap-3 mt-6 pt-5 border-t border-border/30">
            <UiButton variant="ghost" size="sm" :disabled="savingOverride" @click="showOverrideDialog = false">Cancel</UiButton>
            <UiButton size="sm" :disabled="savingOverride" @click="saveOverride">{{ savingOverride ? 'Saving…' : 'Save' }}</UiButton>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- Add Secret override -->
    <Teleport to="body">
      <div v-if="showSecretOverrideDialog" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="showSecretOverrideDialog = false" />
        <div class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6">
          <h3 class="text-[15px] font-semibold mb-5">Add Secret override</h3>
          <p
            v-if="secretOverrideDialogError"
            class="mb-4 rounded-lg bg-red-500/10 border border-red-500/20 px-3 py-2 text-[12px] text-red-700 dark:text-red-400"
          >
            {{ secretOverrideDialogError }}
          </p>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium mb-1">Secret <span class="text-red-500">*</span></label>
              <select v-model="overrideSecretId" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50">
                <option v-for="s in unboundSecrets" :key="s.id" :value="s.id">{{ s.name }} ({{ s.kind }})</option>
              </select>
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Override value <span class="text-red-500">*</span></label>
              <textarea
                v-model="overrideSecretValue"
                rows="4"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50 resize-y"
                placeholder="Enter the environment-specific secret value"
                autocomplete="off"
              />
              <p class="mt-1 text-[11px] text-muted-foreground">Stored encrypted. Not shown again after save.</p>
            </div>
          </div>
          <div class="flex justify-end gap-3 mt-6 pt-5 border-t border-border/30">
            <UiButton variant="ghost" size="sm" :disabled="savingSecretOverride" @click="showSecretOverrideDialog = false">Cancel</UiButton>
            <UiButton size="sm" :disabled="savingSecretOverride" @click="saveSecretOverride">{{ savingSecretOverride ? 'Saving…' : 'Save' }}</UiButton>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>
