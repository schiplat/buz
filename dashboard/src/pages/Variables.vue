<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import UiButton from '@/components/ui/UiButton.vue'
import TablePagination from '@/components/ui/TablePagination.vue'
import { Plus, Pencil, Trash2, SlidersHorizontal, GitBranch } from '@lucide/vue'
import { variablesApi, type Variable } from '@/api/variables'
import GitHubBindChips from '@/components/github/GitHubBindChips.vue'
import GitHubBindDialog from '@/components/github/GitHubBindDialog.vue'
import { useAuthStore } from '@/stores/auth'
import { useClientSort } from '@/composables/useClientSort'
import { useClientPagination } from '@/composables/useClientPagination'
import { useAppDialog } from '@/composables/useAppDialog'

const auth = useAuthStore()
const dialog = useAppDialog()

const variables = ref<Variable[]>([])
const loading = ref(true)
const searchQuery = ref('')
/** '' | 'needs_push' — pending / stale / failed binds. */
const filterGithub = ref('')

const filteredVars = computed(() => {
  let source = variables.value
  if (filterGithub.value === 'needs_push') {
    source = source.filter((v) => needsGithubPush(v))
  }
  const q = searchQuery.value.trim().toLowerCase()
  if (!q) return source
  return source.filter((v) => {
    return [v.name, v.value].filter(Boolean).join(' ').toLowerCase().includes(q)
  })
})

const { sorted, toggleSort, sortIndicator } = useClientSort(filteredVars, {
  initialKey: 'name',
  initialDir: 'asc',
  getValue: (row, key) => {
    switch (key) {
      case 'name': return row.name
      case 'value': return row.value
      case 'github_status': return githubStatusValue(row)
      case 'updated_at': return row.updated_at || ''
      default: return ''
    }
  },
})

const { page, pageSize, pageCount, total, pageItems, rangeLabel } = useClientPagination(sorted)

// ── GitHub Sync bindings (platform admin only — variables are platform-level) ──
const showGithubDialog = ref(false)
const githubTarget = ref<Variable | null>(null)

/** Sorting summary: none < failed < stale < pending < ok. */
function githubStatusValue(v: Variable): string {
  if (!v.github_binds?.length) return ''
  if (v.github_binds.some((b) => b.last_status === 'failed')) return 'failed'
  if (v.github_binds.some((b) => b.last_status === 'stale')) return 'stale'
  if (v.github_binds.some((b) => b.last_status === 'pending')) return 'pending'
  return 'ok'
}

function needsGithubPush(v: Variable): boolean {
  return !!v.github_binds?.some(
    (b) => b.last_status === 'pending' || b.last_status === 'stale' || b.last_status === 'failed',
  )
}

function openGithub(v: Variable) {
  githubTarget.value = v
  showGithubDialog.value = true
}

function onGithubChanged() {
  void loadVars()
}

// ── Create / Edit dialog ──
const showDialog = ref(false)
const editing = ref<Variable | null>(null)
const formName = ref('')
const formValue = ref('')
const saving = ref(false)

function openCreate() {
  editing.value = null
  formName.value = ''
  formValue.value = ''
  showDialog.value = true
}

function openEdit(v: Variable) {
  editing.value = v
  formName.value = v.name
  formValue.value = ''
  showDialog.value = true
}

async function handleSave() {
  saving.value = true
  try {
    if (editing.value) {
      const target = editing.value
      const res = await variablesApi.update(editing.value.id, { value: formValue.value })
      showDialog.value = false
      await loadVars()
      const stale = res.github_binds_stale ?? 0
      if (stale > 0 && auth.isPlatformAdmin) {
        const go = await dialog.confirm(
          `${stale} GitHub bind${stale === 1 ? '' : 's'} marked as needs push (inherit only; custom overrides unchanged). Push now?`,
          { title: 'GitHub sync', confirmLabel: 'Push now', cancelLabel: 'Later' },
        )
        if (go) {
          const refreshed = variables.value.find((v) => v.id === target.id) || target
          openGithub(refreshed)
        }
      }
    } else {
      await variablesApi.create({ name: formName.value.trim(), value: formValue.value })
      showDialog.value = false
      await loadVars()
    }
  } catch (e: any) {
    // Surface the backend's message instead of letting the unhandled rejection
    // crash the page via ErrorBoundary (same pattern as Secrets.vue).
    await dialog.alert(e?.response?.data?.message || e?.message || 'Failed to save variable')
  } finally {
    saving.value = false
  }
}

async function handleDelete(v: Variable) {
  if (!(await dialog.confirm(`Delete variable "${v.name}"?`, {
    title: 'Delete variable',
    confirmLabel: 'Delete',
    danger: true,
  }))) return
  try {
    await variablesApi.delete(v.id)
    await loadVars()
  } catch (e: any) {
    await dialog.alert(e?.response?.data?.message || e?.message || 'Failed to delete variable')
  }
}

async function loadVars() {
  try {
    variables.value = await variablesApi.list()
  } catch {
    // ignore
  } finally {
    loading.value = false
  }
}

onMounted(loadVars)
</script>

<template>
  <div class="space-y-6">
    <!-- Toolbar (no PageHeader — the tab bar above is the page header) -->
    <div class="flex items-center gap-3 flex-wrap">
      <p class="type-meta min-w-0 basis-full sm:basis-auto sm:max-w-xl">
        Non-sensitive shared config as <code class="mono-chip text-[10px]">$NAME</code> in steps. Override via Environments or GitHub Sync. Do not store secrets here.
      </p>
      <UiButton size="sm" class="sm:ml-auto" @click="openCreate"><Plus class="h-4 w-4" /> Add Variable</UiButton>
    </div>

    <div v-if="loading" class="text-center py-12 text-muted-foreground text-sm">Loading…</div>
    <div v-else class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-xs">
      <div v-if="variables.length === 0" class="flex flex-col items-center justify-center py-16 text-muted-foreground">
        <SlidersHorizontal class="h-10 w-10 mb-3 text-muted-foreground/20" />
        <p class="text-sm">No variables configured yet</p>
        <p class="text-xs mt-1 text-muted-foreground/60">
          After creating variables, bind them in Pipeline → Advanced → Variables
          (or YAML <code class="mono-chip text-[10px]">variables: [NAME]</code>).
        </p>
      </div>
      <template v-else>
        <!-- Filter bar -->
        <div class="px-5 py-3 border-b border-border/30 flex items-center gap-3 flex-wrap">
          <input
            v-model="searchQuery"
            type="text"
            placeholder="Search variables…"
            class="w-full max-w-56 rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-hidden transition-colors focus:border-primary/50 focus:ring-1 focus:ring-primary/10"
          />
          <select v-model="filterGithub" class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-hidden transition-colors focus:border-primary/50" title="Filter by GitHub sync status">
            <option value="">All GitHub status</option>
            <option value="needs_push">Needs push</option>
          </select>
          <span v-if="filteredVars.length !== variables.length" class="text-[10px] text-muted-foreground ml-auto">
            {{ filteredVars.length }} of {{ variables.length }}
          </span>
        </div>
        <div class="overflow-x-auto">
          <table class="w-full text-[13px]">
            <thead>
              <tr class="border-b border-border/30 bg-muted/10">
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('name')">
                    Name
                    <span class="font-mono text-xs">{{ sortIndicator('name') }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('value')">
                    Default
                    <span class="font-mono text-xs">{{ sortIndicator('value') }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('github_status')">
                    GitHub Sync
                    <span class="font-mono text-xs">{{ sortIndicator('github_status') }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('updated_at')">
                    Updated
                    <span class="font-mono text-xs">{{ sortIndicator('updated_at') }}</span>
                  </button>
                </th>
                <th class="text-right px-5 py-2.5 w-20" />
              </tr>
            </thead>
            <tbody class="divide-y divide-border/20">
              <tr v-for="v in pageItems" :key="v.id" class="hover:bg-muted/5 transition-colors">
                <td class="px-5 py-3">
                  <span
                    class="font-mono text-[12px] font-medium leading-snug text-foreground/90 break-all"
                    :title="v.name"
                  >{{ v.name }}</span>
                </td>
                <td class="px-5 py-3">
                  <code class="mono-chip text-xs">{{ v.value }}</code>
                </td>
                <td v-if="auth.isPlatformAdmin" class="px-5 py-3">
                  <GitHubBindChips
                    v-if="v.github_binds && v.github_binds.length > 0"
                    :binds="v.github_binds"
                    @open="openGithub(v)"
                  />
                  <span v-else class="text-muted-foreground/50" title="No GitHub repo bound">—</span>
                </td>
                <td class="px-5 py-3 text-xs text-muted-foreground whitespace-nowrap">{{ v.updated_at?.slice(0, 10) || '—' }}</td>
                <td class="px-5 py-3 text-right whitespace-nowrap">
                  <div class="flex items-center justify-end gap-1">
                    <UiButton v-if="auth.isPlatformAdmin" variant="ghost" size="sm" @click="openGithub(v)" title="GitHub Actions sync"><GitBranch class="h-3.5 w-3.5" /></UiButton>
                    <UiButton variant="ghost" size="sm" @click="openEdit(v)" title="Edit"><Pencil class="h-3.5 w-3.5" /></UiButton>
                    <UiButton variant="ghost" size="sm" @click="handleDelete(v)" title="Delete"><Trash2 class="h-3.5 w-3.5 text-red-500" /></UiButton>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <TablePagination v-model:page="page" v-model:pageSize="pageSize" :total="total" :page-count="pageCount" :range-label="rangeLabel" />
      </template>
    </div>

    <!-- Dialog -->
    <Teleport to="body">
      <div v-if="showDialog" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-xs" @click="showDialog = false" />
        <div class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6">
          <h3 class="text-[15px] font-semibold mb-5">{{ editing ? 'Edit Variable' : 'New Variable' }}</h3>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Name <span class="text-red-500">*</span></label>
              <input
                v-model="formName"
                :disabled="!!editing"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-hidden focus:border-primary/50 focus:ring-1 focus:ring-primary/20 disabled:bg-muted/30 disabled:cursor-not-allowed"
                placeholder="DOCKER_REGISTRY"
              />
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">
                Default value <span class="text-red-500">*</span>
                <span v-if="editing" class="text-muted-foreground font-normal ml-1">(leave empty to keep current)</span>
              </label>
              <textarea
                v-model="formValue"
                rows="6"
                spellcheck="false"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-hidden focus:border-primary/50 focus:ring-1 focus:ring-primary/20 resize-y min-h-30"
                :placeholder="editing ? '(keep current value)' : 'e.g. registry.example.com'"
              />
            </div>
          </div>
          <div class="flex items-center justify-end gap-3 mt-6 pt-5 border-t border-border/30">
            <UiButton variant="ghost" size="sm" @click="showDialog = false" :disabled="saving">Cancel</UiButton>
            <UiButton size="sm" @click="handleSave" :disabled="saving">{{ saving ? 'Saving…' : (editing ? 'Update' : 'Create') }}</UiButton>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- GitHub Sync bindings dialog (platform admin) -->
    <GitHubBindDialog
      v-model:show="showGithubDialog"
      resource-kind="variable"
      :resource-id="githubTarget?.id || ''"
      :resource-name="githubTarget?.name || ''"
      :default-value="githubTarget?.value || ''"
      :can-manage="auth.isPlatformAdmin"
      @changed="onGithubChanged"
    />
  </div>
</template>
