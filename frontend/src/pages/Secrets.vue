<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import TablePagination from '@/components/ui/TablePagination.vue'
import { Plus, Pencil, Trash2, Shield } from '@lucide/vue'
import { secretsApi, type Secret } from '@/api/secrets'
import { teamsApi, type Team } from '@/api/teams'
import { useAuthStore } from '@/stores/auth'
import { useClientSort } from '@/composables/useClientSort'
import { useClientPagination } from '@/composables/useClientPagination'

const auth = useAuthStore()
const secrets = ref<Secret[]>([])
const teams = ref<Team[]>([])
const loading = ref(true)
const searchQuery = ref('')
const filterTeam = ref('')

const filteredSecrets = computed(() => {
  let source = secrets.value.filter((s) => auth.matchesActiveTeam(s.team_id))
  if (filterTeam.value) source = source.filter((s) => s.team_id === filterTeam.value)
  const q = searchQuery.value.trim().toLowerCase()
  if (!q) return source
  return source.filter((s) => {
    const teamName = teams.value.find(t => t.id === s.team_id)?.name || s.team_id || ''
    return [s.name, s.value, s.owner_group, teamName].filter(Boolean).join(' ').toLowerCase().includes(q)
  })
})

const { sorted, toggleSort, sortIndicator } = useClientSort(filteredSecrets, {
  initialKey: 'name',
  initialDir: 'asc',
  getValue: (row, key) => {
    switch (key) {
      case 'name': return row.name
      case 'value': return row.value
      case 'team': {
        const t = teams.value.find(x => x.id === row.team_id)
        return t ? t.name : (row.team_id || '')
      }
      case 'updated_at': return row.updated_at || ''
      default: return ''
    }
  },
})

const { page, pageSize, pageCount, total, pageItems, rangeLabel } = useClientPagination(sorted)

// ── Create / Edit dialog ──
const showDialog = ref(false)
const editing = ref<Secret | null>(null)
const formName = ref('')
const formValue = ref('')
const formTeamId = ref('')
const saving = ref(false)
const showValue = ref(false)

function openCreate() {
  editing.value = null
  formName.value = ''
  formValue.value = ''
  formTeamId.value = auth.preferredTeamId || teams.value[0]?.id || ''
  showValue.value = false
  showDialog.value = true
}

function openEdit(s: Secret) {
  editing.value = s
  formName.value = s.name
  formValue.value = ''
  formTeamId.value = s.team_id || ''
  showValue.value = false
  showDialog.value = true
}

async function handleSave() {
  if (!formTeamId.value) {
    alert('Team is required')
    return
  }
  saving.value = true
  try {
    if (editing.value) {
      await secretsApi.update(editing.value.id, {
        value: formValue.value,
        team_id: formTeamId.value,
      })
    } else {
      await secretsApi.create({
        name: formName.value.trim(),
        value: formValue.value,
        team_id: formTeamId.value,
      })
    }
    showDialog.value = false
    await loadSecrets()
  } finally {
    saving.value = false
  }
}

async function handleDelete(s: Secret) {
  if (!confirm(`Delete secret "${s.name}"? This cannot be undone.`)) return
  await secretsApi.delete(s.id)
  await loadSecrets()
}

async function loadSecrets() {
  try {
    const [s, t] = await Promise.all([secretsApi.list(), teamsApi.list()])
    secrets.value = s
    teams.value = t
  } catch {
    // ignore
  } finally {
    loading.value = false
  }
}

onMounted(loadSecrets)
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      title="Secrets"
      description="Encrypted credentials. Bind to pipelines via matching Team (auto) or Authorized Secrets (explicit). Values are masked in logs."
    >
      <template #actions>
        <UiButton size="sm" @click="openCreate"><Plus class="h-4 w-4" /> Add Secret</UiButton>
      </template>
    </PageHeader>

    <div v-if="loading" class="text-center py-12 text-muted-foreground text-sm">Loading…</div>
    <div v-else class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
      <!-- Filter bar -->
      <div class="px-5 py-3 border-b border-border/30 flex items-center gap-3 flex-wrap">
        <input
          v-model="searchQuery"
          type="text"
          placeholder="Search secrets…"
          class="w-full max-w-44 rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50 focus:ring-1 focus:ring-primary/10"
        />
        <select v-model="filterTeam" class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50">
          <option value="">All teams</option>
          <option v-for="t in teams" :key="t.id" :value="t.id">{{ t.name }}</option>
        </select>
        <span v-if="filteredSecrets.length !== secrets.length" class="text-[10px] text-muted-foreground ml-auto">
          {{ filteredSecrets.length }} of {{ secrets.length }}
        </span>
      </div>
      <template v-if="filteredSecrets.length > 0">
        <div class="overflow-x-auto">
          <table class="w-full text-[13px]">
            <thead>
              <tr class="border-b border-border/30 bg-muted/10">
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('name')">
                    Name
                    <span class="font-mono text-xs">{{ sortIndicator('name') || '↕' }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('value')">
                    Value
                    <span class="font-mono text-xs">{{ sortIndicator('value') || '↕' }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('team')">
                    Team
                    <span class="font-mono text-xs">{{ sortIndicator('team') || '↕' }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('updated_at')">
                    Updated
                    <span class="font-mono text-xs">{{ sortIndicator('updated_at') || '↕' }}</span>
                  </button>
                </th>
                <th class="text-right px-5 py-2.5 w-20" />
              </tr>
            </thead>
            <tbody class="divide-y divide-border/20">
              <tr v-for="s in pageItems" :key="s.id" class="hover:bg-muted/5 transition-colors">
                <td class="px-5 py-3 font-mono text-sm font-semibold whitespace-nowrap">{{ s.name }}</td>
                <td class="px-5 py-3">
                  <code class="mono-chip text-xs select-all">{{ s.value }}</code>
                </td>
                <td class="px-5 py-3 text-xs text-muted-foreground whitespace-nowrap">{{ s.owner_group || s.team_id || '—' }}</td>
                <td class="px-5 py-3 text-xs text-muted-foreground whitespace-nowrap">{{ s.updated_at?.slice(0, 10) || '—' }}</td>
                <td class="px-5 py-3 text-right whitespace-nowrap">
                  <div class="flex items-center justify-end gap-1">
                    <UiButton variant="ghost" size="sm" @click="openEdit(s)" title="Edit"><Pencil class="h-3.5 w-3.5" /></UiButton>
                    <UiButton variant="ghost" size="sm" @click="handleDelete(s)" title="Delete"><Trash2 class="h-3.5 w-3.5 text-red-500" /></UiButton>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <TablePagination v-model:page="page" v-model:pageSize="pageSize" :total="total" :page-count="pageCount" :range-label="rangeLabel" />
      </template>
      <div v-else class="flex flex-col items-center justify-center py-16 text-muted-foreground">
        <Shield class="h-10 w-10 mb-3 text-muted-foreground/20" />
        <p class="text-sm">{{ secrets.length === 0 ? 'No secrets configured yet' : 'No secrets in the selected team' }}</p>
        <p class="text-xs mt-1 text-muted-foreground/60">
          Create secrets here, then bind them in Pipeline → Advanced → Authorized Secrets,
          or assign the same <code class="mono-chip text-[10px]">Team</code> for auto-access.
        </p>
      </div>
    </div>

    <!-- Dialog -->
    <Teleport to="body">
      <div v-if="showDialog" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="showDialog = false" />
        <div class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6">
          <h3 class="text-[15px] font-semibold mb-5">{{ editing ? 'Update Secret' : 'New Secret' }}</h3>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Name <span class="text-red-400">*</span></label>
              <input
                v-model="formName"
                :disabled="!!editing"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20 disabled:bg-muted/30 disabled:cursor-not-allowed"
                placeholder="DOCKER_REGISTRY_PASSWORD"
              />
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Team <span class="text-red-500">*</span></label>
              <select
                v-model="formTeamId"
                class="w-full appearance-none rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20 cursor-pointer"
              >
                <option disabled value="">Select a team</option>
                <option v-for="t in teams" :key="t.id" :value="t.id">{{ t.name }} ({{ t.slug }})</option>
              </select>
              <p class="text-[10px] text-muted-foreground mt-1">Pipelines in the same team auto-receive this secret.</p>
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">
                Value <span class="text-red-400">*</span>
                <span v-if="editing" class="text-muted-foreground font-normal ml-1">(leave empty to keep current)</span>
              </label>
              <div class="relative">
                <input
                  v-model="formValue"
                  :type="showValue ? 'text' : 'password'"
                  class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 pr-14 text-sm font-mono outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20"
                  :placeholder="editing ? 'new value' : '••••••••'"
                />
                <button type="button" class="absolute right-2 top-1/2 -translate-y-1/2 text-[11px] text-muted-foreground hover:text-foreground transition-colors cursor-pointer" @click="showValue = !showValue">
                  {{ showValue ? 'Hide' : 'Show' }}
                </button>
              </div>
            </div>
          </div>
          <div class="flex items-center justify-end gap-3 mt-6 pt-5 border-t border-border/30">
            <UiButton variant="ghost" size="sm" @click="showDialog = false" :disabled="saving">Cancel</UiButton>
            <UiButton size="sm" @click="handleSave" :disabled="saving">{{ saving ? 'Saving…' : (editing ? 'Update' : 'Create') }}</UiButton>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>
