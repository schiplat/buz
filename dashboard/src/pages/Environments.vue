<script setup lang="ts">
import { ref, onMounted, watch } from 'vue'
import { useRouter } from 'vue-router'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import TablePagination from '@/components/ui/TablePagination.vue'
import { Plus, Pencil, Trash2, Layers } from '@lucide/vue'
import { environmentsApi, type Environment } from '@/api/environments'
import { useAuthStore } from '@/stores/auth'
import { formatLocalDateTime } from '@/utils/time'
import { useAppDialog } from '@/composables/useAppDialog'
import { useClientPagination } from '@/composables/useClientPagination'

const auth = useAuthStore()
const dialog = useAppDialog()
const router = useRouter()

const environments = ref<Environment[]>([])
const loading = ref(true)
const error = ref('')

const { page, pageSize, pageCount, total, pageItems, rangeLabel } = useClientPagination(environments)

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
const formDeploymentConcurrency = ref<'off' | 'queue' | 'cancel_in_progress'>('off')
const savingEnv = ref(false)

const ROLE_OPTIONS = ['developer', 'maintainer', 'owner'] as const

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

function openDetail(env: Environment) {
  void router.push({ name: 'environment-detail', params: { id: env.id } })
}

function resetProtectionForm() {
  formProtectionEnabled.value = false
  formMinTriggerRole.value = 'developer'
  formMinApproveRole.value = 'maintainer'
  formRequireApprovals.value = 1
  formForbidSelfApprove.value = true
  formApprovalTimeoutSeconds.value = 86400
  formDeploymentConcurrency.value = 'off'
}

function loadProtectionForm(env: Environment) {
  formProtectionEnabled.value = !!env.protection_enabled
  formMinTriggerRole.value = (env.min_trigger_role as string) || 'developer'
  formMinApproveRole.value = (env.min_approve_role as string) || 'maintainer'
  formRequireApprovals.value = Math.max(1, env.require_approvals ?? 1)
  formForbidSelfApprove.value = env.forbid_self_approve !== false
  formApprovalTimeoutSeconds.value = env.approval_timeout_seconds ?? 86400
  formDeploymentConcurrency.value = env.deployment_concurrency || 'off'
}

function protectionPayload() {
  return {
    protection_enabled: formProtectionEnabled.value,
    min_trigger_role: formMinTriggerRole.value,
    min_approve_role: formMinApproveRole.value,
    require_approvals: Math.max(1, Number(formRequireApprovals.value) || 1),
    forbid_self_approve: formForbidSelfApprove.value,
    approval_timeout_seconds: Math.max(60, Number(formApprovalTimeoutSeconds.value) || 86400),
    deployment_concurrency: formDeploymentConcurrency.value,
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
      showEnvDialog.value = false
      await loadList()
    } else {
      const created = await environmentsApi.create({
        name: formName.value.trim(),
        slug: formSlug.value.trim(),
        description: formDescription.value,
        ...protection,
      })
      showEnvDialog.value = false
      void router.push({ name: 'environment-detail', params: { id: created.id } })
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
    await loadList()
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to delete'
  }
}

onMounted(() => {
  void loadList()
})
</script>

<template>
  <div class="flex flex-col gap-5">
    <PageHeader
      title="Environments"
      description="Named scopes that override Variable and Secret defaults at Trigger time. Orthogonal to GitHub Sync per-repo overrides."
    >
      <template #actions>
        <UiButton
          v-if="auth.isPlatformAdmin"
          size="sm"
          @click="openCreateEnv"
        >
          <Plus class="h-3.5 w-3.5" /> New environment
        </UiButton>
      </template>
    </PageHeader>

    <p v-if="error" class="rounded-lg bg-red-500/10 border border-red-500/20 px-3 py-2 text-[12px] text-red-700 dark:text-red-400">
      {{ error }}
    </p>

    <div v-if="loading" class="text-center py-12 text-muted-foreground text-sm">Loading…</div>

    <div v-else class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-xs">
      <div v-if="environments.length === 0" class="flex flex-col items-center justify-center py-16 text-muted-foreground">
        <Layers class="h-10 w-10 mb-3 text-muted-foreground/20" />
        <p class="text-sm">No environments yet</p>
        <p class="text-xs mt-1 text-muted-foreground/60">Create staging / production to override Variable and Secret defaults per run.</p>
      </div>
      <template v-else>
        <table class="w-full text-[13px]">
          <thead>
            <tr class="border-b border-border/30 bg-muted/10">
              <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Name</th>
              <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Slug</th>
              <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Protection</th>
              <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Deploy concurrency</th>
              <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Hosts</th>
              <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Overrides</th>
              <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Updated</th>
              <th class="text-right px-5 py-2.5 w-28" />
            </tr>
          </thead>
          <tbody class="divide-y divide-border/20">
            <tr
              v-for="e in pageItems"
              :key="e.id"
              class="hover:bg-muted/5 transition-colors cursor-pointer"
              @click="openDetail(e)"
            >
              <td class="px-5 py-3">
                <span
                  class="font-mono text-[12px] font-medium leading-snug text-foreground/90 break-all"
                  :title="e.name"
                >{{ e.name }}</span>
              </td>
              <td class="px-5 py-3 font-mono text-xs">{{ e.slug }}</td>
              <td class="px-5 py-3">
                <span
                  v-if="e.protection_enabled"
                  class="inline-flex items-center rounded-full border border-amber-500/30 bg-amber-500/10 px-2 py-0.5 text-[11px] font-medium text-amber-700 dark:text-amber-400"
                >Protected</span>
                <span v-else class="text-xs text-muted-foreground/40">—</span>
              </td>
              <td class="px-5 py-3">
                <span class="mono-chip text-xs">{{ e.deployment_concurrency || 'off' }}</span>
              </td>
              <td class="px-5 py-3 text-xs text-muted-foreground">
                {{ e.host_allowlist_count ? `${e.host_allowlist_count} entries` : 'Unrestricted' }}
              </td>
              <td class="px-5 py-3 text-muted-foreground text-xs">
                <span title="Variable overrides">V:{{ e.var_override_count ?? 0 }}</span>
                <span class="mx-1 text-muted-foreground/40">·</span>
                <span title="Secret overrides">S:{{ e.secret_override_count ?? 0 }}</span>
              </td>
              <td class="px-5 py-3 text-muted-foreground text-xs">{{ e.updated_at ? formatLocalDateTime(e.updated_at) : '—' }}</td>
              <td class="px-5 py-3 text-right" @click.stop>
                <div v-if="auth.isPlatformAdmin" class="inline-flex gap-1">
                  <UiButton variant="ghost" size="sm" @click="openEditEnv(e)" title="Environment host allowlist"
              ><Pencil class="h-3.5 w-3.5" /></UiButton>
                  <UiButton variant="ghost" size="sm" @click="deleteEnv(e)" title="Delete"><Trash2 class="h-3.5 w-3.5 text-red-500" /></UiButton>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
        <TablePagination
          v-model:page="page"
          v-model:pageSize="pageSize"
          :total="total"
          :page-count="pageCount"
          :range-label="rangeLabel"
        />
      </template>
    </div>

    <!-- Create / Edit environment -->
    <Teleport to="body">
      <div v-if="showEnvDialog" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-xs" @click="showEnvDialog = false" />
        <div class="relative z-10 w-full max-w-lg mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6 max-h-[90vh] overflow-y-auto">
          <h3 class="text-[15px] font-semibold mb-5">{{ editingEnv ? 'Edit Environment' : 'New Environment' }}</h3>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium mb-1">Name <span class="text-red-500">*</span></label>
              <input v-model="formName" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50" placeholder="Production" />
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Slug <span class="text-red-500">*</span></label>
              <input v-model="formSlug" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-hidden focus:border-primary/50" placeholder="production" />
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Description</label>
              <input v-model="formDescription" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50" placeholder="Optional" />
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
            <div>
              <label class="block text-[12px] font-medium mb-1">Deployment concurrency</label>
              <select v-model="formDeploymentConcurrency" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50">
                <option value="off">off — do not serialize across pipelines</option>
                <option value="queue">queue — FIFO across pipelines</option>
                <option value="cancel_in_progress">cancel_in_progress — replace active deployment</option>
              </select>
            </div>
            <template v-if="formProtectionEnabled">
                <div class="grid grid-cols-2 gap-3">
                  <div>
                    <label class="block text-[12px] font-medium mb-1">Min trigger role <span class="text-red-500">*</span></label>
                    <select v-model="formMinTriggerRole" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50">
                      <option v-for="r in ROLE_OPTIONS" :key="r" :value="r">{{ r }}</option>
                    </select>
                  </div>
                  <div>
                    <label class="block text-[12px] font-medium mb-1">Min approve role <span class="text-red-500">*</span></label>
                    <select v-model="formMinApproveRole" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50">
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
                      class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50"
                    />
                  </div>
                  <div>
                    <label class="block text-[12px] font-medium mb-1">Timeout (seconds) <span class="text-red-500">*</span></label>
                    <input
                      v-model.number="formApprovalTimeoutSeconds"
                      type="number"
                      min="60"
                      class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-hidden focus:border-primary/50"
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
  </div>
</template>
