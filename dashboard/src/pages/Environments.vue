<script setup lang="ts">
import { ref, computed, onMounted, watch } from 'vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import { Plus, Pencil, Trash2, Layers, ArrowLeft } from '@lucide/vue'
import {
  environmentsApi,
  type Environment,
  type EnvironmentOverride,
  type EnvironmentSecretOverride,
} from '@/api/environments'
import { variablesApi, type Variable } from '@/api/variables'
import { secretsApi, type Secret } from '@/api/secrets'
import { useAuthStore } from '@/stores/auth'
import { formatLocalDateTime } from '@/utils/time'
import { useAppDialog } from '@/composables/useAppDialog'

const auth = useAuthStore()
const dialog = useAppDialog()

const environments = ref<Environment[]>([])
const loading = ref(true)
const error = ref('')

const selected = ref<Environment | null>(null)
const overrides = ref<EnvironmentOverride[]>([])
const secretOverrides = ref<EnvironmentSecretOverride[]>([])
const allVars = ref<Variable[]>([])
const allSecrets = ref<Secret[]>([])
const detailLoading = ref(false)

const showEnvDialog = ref(false)
const editingEnv = ref<Environment | null>(null)
const formName = ref('')
const formSlug = ref('')
const formDescription = ref('')
const formProtectionEnabled = ref(false)
const formMinTriggerRole = ref('developer')
const formMinApproveRole = ref('maintainer')
const formRequireApprovals = ref(1)
const formForbidSelfApprove = ref(true)
const formApprovalTimeoutSeconds = ref(86400)
const savingEnv = ref(false)

const ROLE_OPTIONS = ['developer', 'maintainer', 'owner'] as const

const showOverrideDialog = ref(false)
const overrideVarId = ref('')
const overrideValue = ref('')
const savingOverride = ref(false)

const showSecretOverrideDialog = ref(false)
const overrideSecretId = ref('')
const overrideSecretValue = ref('')
const savingSecretOverride = ref(false)

const unboundVars = computed(() => {
  const taken = new Set(overrides.value.map((o) => o.var_id))
  return allVars.value.filter((v) => !taken.has(v.id))
})

const unboundSecrets = computed(() => {
  const taken = new Set(secretOverrides.value.map((o) => o.secret_id))
  return allSecrets.value.filter((s) => !taken.has(s.id))
})

async function loadList() {
  loading.value = true
  error.value = ''
  try {
    environments.value = await environmentsApi.list()
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to load environments'
  } finally {
    loading.value = false
  }
}

async function openDetail(env: Environment) {
  selected.value = env
  detailLoading.value = true
  error.value = ''
  try {
    const [detail, vars, secrets] = await Promise.all([
      environmentsApi.get(env.id),
      auth.isPlatformAdmin ? variablesApi.list() : Promise.resolve([] as Variable[]),
      auth.isPlatformAdmin ? secretsApi.list() : Promise.resolve([] as Secret[]),
    ])
    selected.value = detail
    overrides.value = detail.overrides || []
    secretOverrides.value = detail.secret_overrides || []
    allVars.value = vars
    allSecrets.value = secrets
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to load environment'
  } finally {
    detailLoading.value = false
  }
}

function backToList() {
  selected.value = null
  overrides.value = []
  secretOverrides.value = []
  void loadList()
}

function resetProtectionForm() {
  formProtectionEnabled.value = false
  formMinTriggerRole.value = 'developer'
  formMinApproveRole.value = 'maintainer'
  formRequireApprovals.value = 1
  formForbidSelfApprove.value = true
  formApprovalTimeoutSeconds.value = 86400
}

function loadProtectionForm(env: Environment) {
  formProtectionEnabled.value = !!env.protection_enabled
  formMinTriggerRole.value = (env.min_trigger_role as string) || 'developer'
  formMinApproveRole.value = (env.min_approve_role as string) || 'maintainer'
  formRequireApprovals.value = Math.max(1, env.require_approvals ?? 1)
  formForbidSelfApprove.value = env.forbid_self_approve !== false
  formApprovalTimeoutSeconds.value = env.approval_timeout_seconds ?? 86400
}

function protectionPayload() {
  return {
    protection_enabled: formProtectionEnabled.value,
    min_trigger_role: formMinTriggerRole.value,
    min_approve_role: formMinApproveRole.value,
    require_approvals: Math.max(1, Number(formRequireApprovals.value) || 1),
    forbid_self_approve: formForbidSelfApprove.value,
    approval_timeout_seconds: Math.max(60, Number(formApprovalTimeoutSeconds.value) || 86400),
  }
}

function openCreateEnv() {
  editingEnv.value = null
  formName.value = ''
  formSlug.value = ''
  formDescription.value = ''
  resetProtectionForm()
  showEnvDialog.value = true
}

function openEditEnv(env: Environment) {
  editingEnv.value = env
  formName.value = env.name
  formSlug.value = env.slug
  formDescription.value = env.description || ''
  loadProtectionForm(env)
  showEnvDialog.value = true
}

watch(formName, (n) => {
  if (editingEnv.value) return
  formSlug.value = n
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 64)
})

async function saveEnv() {
  if (!formName.value.trim() || !formSlug.value.trim()) {
    error.value = 'Name and slug are required'
    return
  }
  savingEnv.value = true
  error.value = ''
  try {
    const protection = auth.isPlatformAdmin ? protectionPayload() : {}
    if (editingEnv.value) {
      await environmentsApi.update(editingEnv.value.id, {
        name: formName.value.trim(),
        slug: formSlug.value.trim(),
        description: formDescription.value,
        ...protection,
      })
    } else {
      await environmentsApi.create({
        name: formName.value.trim(),
        slug: formSlug.value.trim(),
        description: formDescription.value,
        ...protection,
      })
    }
    showEnvDialog.value = false
    if (selected.value && editingEnv.value?.id === selected.value.id) {
      await openDetail({ ...selected.value, name: formName.value.trim(), slug: formSlug.value.trim() })
    } else {
      await loadList()
    }
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to save environment'
  } finally {
    savingEnv.value = false
  }
}

async function deleteEnv(env: Environment) {
  if (!(await dialog.confirm(
    `Delete environment "${env.name}"? Overrides are removed; jobs keep history with a cleared link.`,
    { title: 'Delete environment', confirmLabel: 'Delete', danger: true },
  ))) return
  error.value = ''
  try {
    await environmentsApi.delete(env.id)
    if (selected.value?.id === env.id) backToList()
    else await loadList()
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to delete'
  }
}

function openAddOverride() {
  overrideVarId.value = unboundVars.value[0]?.id || ''
  overrideValue.value = unboundVars.value[0]?.value || ''
  showOverrideDialog.value = true
}

watch(overrideVarId, (id) => {
  const v = allVars.value.find((x) => x.id === id)
  if (v && showOverrideDialog.value) overrideValue.value = v.value
})

async function saveOverride() {
  if (!selected.value || !overrideVarId.value) return
  savingOverride.value = true
  error.value = ''
  try {
    await environmentsApi.upsertOverride(selected.value.id, {
      var_id: overrideVarId.value,
      value: overrideValue.value,
    })
    showOverrideDialog.value = false
    await openDetail(selected.value)
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to save override'
  } finally {
    savingOverride.value = false
  }
}

async function clearOverride(o: EnvironmentOverride) {
  if (!selected.value) return
  if (!(await dialog.confirm(
    `Clear override for ${o.var_name}? This environment will inherit the shared Default again.`,
    { title: 'Clear override', confirmLabel: 'Clear', danger: true },
  ))) return
  error.value = ''
  try {
    await environmentsApi.deleteOverride(selected.value.id, o.var_id)
    await openDetail(selected.value)
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to clear override'
  }
}

function openAddSecretOverride() {
  overrideSecretId.value = unboundSecrets.value[0]?.id || ''
  overrideSecretValue.value = ''
  showSecretOverrideDialog.value = true
}

async function saveSecretOverride() {
  if (!selected.value || !overrideSecretId.value || !overrideSecretValue.value.trim()) {
    error.value = 'Secret and override value are required'
    return
  }
  savingSecretOverride.value = true
  error.value = ''
  try {
    await environmentsApi.upsertSecretOverride(selected.value.id, {
      secret_id: overrideSecretId.value,
      value: overrideSecretValue.value,
    })
    showSecretOverrideDialog.value = false
    overrideSecretValue.value = ''
    await openDetail(selected.value)
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to save secret override'
  } finally {
    savingSecretOverride.value = false
  }
}

async function clearSecretOverride(o: EnvironmentSecretOverride) {
  if (!selected.value) return
  if (!(await dialog.confirm(
    `Clear secret override for ${o.secret_name}? This environment will inherit the shared Default again.`,
    { title: 'Clear secret override', confirmLabel: 'Clear', danger: true },
  ))) return
  error.value = ''
  try {
    await environmentsApi.deleteSecretOverride(selected.value.id, o.secret_id)
    await openDetail(selected.value)
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to clear secret override'
  }
}

onMounted(() => {
  void loadList()
})
</script>

<template>
  <div class="flex flex-col gap-5">
    <PageHeader
      :title="selected ? selected.name : 'Environments'"
      :description="
        selected
          ? `Slug ${selected.slug} — Variable / Secret overrides for this environment (shared Defaults live under Variables & Secrets).`
          : 'Named scopes that override Variable and Secret defaults at Trigger time. Orthogonal to GitHub Sync per-repo overrides.'
      "
    >
      <template #actions>
        <UiButton v-if="selected" variant="outline" size="sm" @click="backToList">
          <ArrowLeft class="h-3.5 w-3.5" /> Back
        </UiButton>
        <UiButton
          v-if="auth.isPlatformAdmin && !selected"
          size="sm"
          @click="openCreateEnv"
        >
          <Plus class="h-3.5 w-3.5" /> New environment
        </UiButton>
        <UiButton
          v-if="auth.isPlatformAdmin && selected"
          size="sm"
          variant="outline"
          @click="openEditEnv(selected)"
        >
          <Pencil class="h-3.5 w-3.5" /> Edit
        </UiButton>
      </template>
    </PageHeader>

    <p v-if="error" class="rounded-lg bg-red-500/10 border border-red-500/20 px-3 py-2 text-[12px] text-red-700 dark:text-red-400">
      {{ error }}
    </p>

    <div v-if="loading || detailLoading" class="text-center py-12 text-muted-foreground text-sm">Loading…</div>

    <!-- List -->
    <div v-else-if="!selected" class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
      <div v-if="environments.length === 0" class="flex flex-col items-center justify-center py-16 text-muted-foreground">
        <Layers class="h-10 w-10 mb-3 text-muted-foreground/20" />
        <p class="text-sm">No environments yet</p>
        <p class="text-xs mt-1 text-muted-foreground/60">Create staging / production to override Variable and Secret defaults per run.</p>
      </div>
      <table v-else class="w-full text-[13px]">
        <thead>
          <tr class="border-b border-border/30 bg-muted/10">
            <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Name</th>
            <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Slug</th>
            <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Protection</th>
            <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Overrides</th>
            <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Updated</th>
            <th class="text-right px-5 py-2.5 w-28" />
          </tr>
        </thead>
        <tbody class="divide-y divide-border/20">
          <tr
            v-for="e in environments"
            :key="e.id"
            class="hover:bg-muted/5 transition-colors cursor-pointer"
            @click="openDetail(e)"
          >
            <td class="px-5 py-3 font-medium">{{ e.name }}</td>
            <td class="px-5 py-3 font-mono text-xs">{{ e.slug }}</td>
            <td class="px-5 py-3">
              <span
                v-if="e.protection_enabled"
                class="inline-flex items-center rounded-full border border-amber-500/30 bg-amber-500/10 px-2 py-0.5 text-[11px] font-medium text-amber-700 dark:text-amber-400"
              >Protected</span>
              <span v-else class="text-xs text-muted-foreground/40">—</span>
            </td>
            <td class="px-5 py-3 text-muted-foreground text-xs">
              <span title="Variable overrides">V:{{ e.var_override_count ?? 0 }}</span>
              <span class="mx-1 text-muted-foreground/40">·</span>
              <span title="Secret overrides">S:{{ e.secret_override_count ?? 0 }}</span>
            </td>
            <td class="px-5 py-3 text-muted-foreground text-xs">{{ e.updated_at ? formatLocalDateTime(e.updated_at) : '—' }}</td>
            <td class="px-5 py-3 text-right" @click.stop>
              <div v-if="auth.isPlatformAdmin" class="inline-flex gap-1">
                <UiButton variant="ghost" size="sm" @click="openEditEnv(e)" title="Edit"><Pencil class="h-3.5 w-3.5" /></UiButton>
                <UiButton variant="ghost" size="sm" @click="deleteEnv(e)" title="Delete"><Trash2 class="h-3.5 w-3.5 text-red-500" /></UiButton>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <!-- Detail / overrides -->
    <div v-else class="space-y-4">
      <p v-if="selected.description" class="text-[12px] text-muted-foreground">{{ selected.description }}</p>

      <!-- Protection -->
      <div class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
        <div class="flex items-center justify-between px-5 py-3 border-b border-border/30">
          <div>
            <p class="text-[12px] font-medium">Protection</p>
            <p class="text-[11px] text-muted-foreground mt-0.5">
              Approval gate before a job with this environment can run.
            </p>
          </div>
          <UiButton
            v-if="auth.isPlatformAdmin"
            size="sm"
            variant="outline"
            @click="openEditEnv(selected)"
          >
            <Pencil class="h-3.5 w-3.5" /> Edit
          </UiButton>
        </div>
        <div class="px-5 py-4">
          <template v-if="selected.protection_enabled">
            <div class="flex flex-wrap gap-x-6 gap-y-3 text-[12px]">
              <div>
                <span class="text-muted-foreground">Status</span>
                <div class="mt-0.5">
                  <span class="inline-flex items-center rounded-full border border-amber-500/30 bg-amber-500/10 px-2 py-0.5 text-[11px] font-medium text-amber-700 dark:text-amber-400">
                    Protected
                  </span>
                </div>
              </div>
              <div>
                <span class="text-muted-foreground">Min trigger role</span>
                <div class="mt-0.5 font-medium">{{ selected.min_trigger_role || 'developer' }}</div>
              </div>
              <div>
                <span class="text-muted-foreground">Min approve role</span>
                <div class="mt-0.5 font-medium">{{ selected.min_approve_role || 'maintainer' }}</div>
              </div>
              <div>
                <span class="text-muted-foreground">Required approvals</span>
                <div class="mt-0.5 font-medium tabular-nums">{{ selected.require_approvals ?? 1 }}</div>
              </div>
              <div>
                <span class="text-muted-foreground">Self-approve</span>
                <div class="mt-0.5 font-medium">{{ selected.forbid_self_approve !== false ? 'Forbidden' : 'Allowed' }}</div>
              </div>
              <div>
                <span class="text-muted-foreground">Timeout</span>
                <div class="mt-0.5 font-medium tabular-nums">{{ selected.approval_timeout_seconds ?? 86400 }}s</div>
              </div>
            </div>
          </template>
          <p v-else class="text-xs text-muted-foreground">
            Not protected — jobs targeting this environment start without approval.
          </p>
        </div>
      </div>

      <!-- Variable overrides -->
      <div class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
        <div class="flex items-center justify-between px-5 py-3 border-b border-border/30">
          <p class="text-[12px] font-medium">Variable overrides</p>
          <UiButton
            v-if="auth.isPlatformAdmin"
            size="sm"
            :disabled="unboundVars.length === 0"
            @click="openAddOverride"
          >
            <Plus class="h-3.5 w-3.5" /> Add override
          </UiButton>
        </div>
        <div v-if="overrides.length === 0" class="px-5 py-10 text-center text-xs text-muted-foreground">
          No overrides — this environment inherits all Variable Defaults.
        </div>
        <table v-else class="w-full text-[13px]">
          <thead>
            <tr class="border-b border-border/30 bg-muted/10">
              <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Variable</th>
              <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Default (shared)</th>
              <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Override</th>
              <th class="text-right px-5 py-2.5 w-24" />
            </tr>
          </thead>
          <tbody class="divide-y divide-border/20">
            <tr v-for="o in overrides" :key="o.var_id" class="hover:bg-muted/5">
              <td class="px-5 py-3 font-mono text-sm font-semibold">{{ o.var_name }}</td>
              <td class="px-5 py-3"><code class="mono-chip text-xs break-all">{{ o.default_value }}</code></td>
              <td class="px-5 py-3"><code class="mono-chip text-xs break-all">{{ o.value }}</code></td>
              <td class="px-5 py-3 text-right">
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

      <!-- Secret overrides -->
      <div class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
        <div class="flex items-center justify-between px-5 py-3 border-b border-border/30">
          <div>
            <p class="text-[12px] font-medium">Secret overrides</p>
            <p class="text-[11px] text-muted-foreground mt-0.5">
              Values are never shown after save. Applied only when the secret is on the job allowlist.
            </p>
          </div>
          <UiButton
            v-if="auth.isPlatformAdmin"
            size="sm"
            :disabled="unboundSecrets.length === 0"
            @click="openAddSecretOverride"
          >
            <Plus class="h-3.5 w-3.5" /> Add override
          </UiButton>
        </div>
        <div v-if="secretOverrides.length === 0" class="px-5 py-10 text-center text-xs text-muted-foreground">
          No overrides — this environment inherits all Secret Defaults.
        </div>
        <table v-else class="w-full text-[13px]">
          <thead>
            <tr class="border-b border-border/30 bg-muted/10">
              <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Secret</th>
              <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Kind</th>
              <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Status</th>
              <th class="text-right px-5 py-2.5 w-24" />
            </tr>
          </thead>
          <tbody class="divide-y divide-border/20">
            <tr v-for="o in secretOverrides" :key="o.secret_id" class="hover:bg-muted/5">
              <td class="px-5 py-3 font-mono text-sm font-semibold">{{ o.secret_name }}</td>
              <td class="px-5 py-3 text-xs text-muted-foreground">{{ o.kind }}</td>
              <td class="px-5 py-3">
                <span class="mono-chip text-xs">Custom</span>
              </td>
              <td class="px-5 py-3 text-right">
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
    </div>

    <!-- Create / Edit environment -->
    <Teleport to="body">
      <div v-if="showEnvDialog" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="showEnvDialog = false" />
        <div class="relative z-10 w-full max-w-lg mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6 max-h-[90vh] overflow-y-auto">
          <h3 class="text-[15px] font-semibold mb-5">{{ editingEnv ? 'Edit Environment' : 'New Environment' }}</h3>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium mb-1">Name <span class="text-red-500">*</span></label>
              <input v-model="formName" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50" placeholder="Production" />
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Slug <span class="text-red-500">*</span></label>
              <input v-model="formSlug" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50" placeholder="production" />
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Description</label>
              <input v-model="formDescription" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50" placeholder="Optional" />
            </div>

            <div v-if="auth.isPlatformAdmin" class="rounded-lg border border-border/40 bg-muted/20 p-3 space-y-3">
              <label class="flex items-start gap-2 cursor-pointer">
                <input v-model="formProtectionEnabled" type="checkbox" class="mt-0.5" />
                <span class="text-[12px] leading-snug">
                  <span class="font-medium">Enable protection</span>
                  <span class="block text-[11px] text-muted-foreground mt-0.5">
                    Jobs using this environment wait for approval before running.
                  </span>
                </span>
              </label>
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
          </div>
          <div class="flex justify-end gap-3 mt-6 pt-5 border-t border-border/30">
            <UiButton variant="ghost" size="sm" :disabled="savingEnv" @click="showEnvDialog = false">Cancel</UiButton>
            <UiButton size="sm" :disabled="savingEnv" @click="saveEnv">{{ savingEnv ? 'Saving…' : 'Save' }}</UiButton>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- Add Variable override -->
    <Teleport to="body">
      <div v-if="showOverrideDialog" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="showOverrideDialog = false" />
        <div class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6">
          <h3 class="text-[15px] font-semibold mb-5">Add Variable override</h3>
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
                type="password"
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
