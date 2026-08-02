<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import TablePagination from '@/components/ui/TablePagination.vue'
import { Plus, Pencil, Trash2, Server, Loader2 } from '@lucide/vue'
import { hostsApi, type Host } from '@/api/hosts'
import { teamsApi, type Team } from '@/api/teams'
import { useAuthStore } from '@/stores/auth'
import { useClientSort } from '@/composables/useClientSort'
import { useClientPagination } from '@/composables/useClientPagination'

const auth = useAuthStore()
const hosts = ref<Host[]>([])
const availableTeams = ref<Team[]>([])
const loading = ref(true)
const searchQuery = ref('')
const filterCredentialType = ref('')

const filteredHosts = computed(() => {
  let source = hosts.value.filter((h) => auth.matchesActiveTeam(h.team_id, { includeShared: true }))
  if (filterCredentialType.value) source = source.filter((h) => h.credential_type === filterCredentialType.value)
  const q = searchQuery.value.trim().toLowerCase()
  if (!q) return source
  return source.filter((h) => {
    const teamName = availableTeams.value.find(t => t.id === h.team_id)?.name || h.team_id || ''
    return [h.name, h.host, String(h.port), h.username, h.credential_type, teamName, ...(h.labels || [])]
      .filter(Boolean)
      .join(' ')
      .toLowerCase()
      .includes(q)
  })
})

const { sorted, toggleSort, sortIndicator } = useClientSort(filteredHosts, {
  initialKey: 'name',
  initialDir: 'asc',
  getValue: (row, key) => {
    switch (key) {
      case 'name': return row.name
      case 'host': return row.host
      case 'port': return row.port
      case 'username': return row.username
      case 'credential_type': return row.credential_type
      case 'team': {
        const t = availableTeams.value.find(x => x.id === row.team_id)
        return t ? t.name : (row.team_id || '')
      }
      default: return ''
    }
  },
})

const { page, pageSize, pageCount, total, pageItems, rangeLabel } = useClientPagination(sorted)

// ── Create / Edit dialog ──
const showDialog = ref(false)
const editing = ref<Host | null>(null)
const form = ref({ name: '', host: '', port: 22, username: '', credential_type: 'key_path', credential_data: '', credential_passphrase: '', labels: '', team_id: '' })
const saving = ref(false)
const testing = ref(false)
const testResult = ref<{ ok: boolean; message: string } | null>(null)

function openCreate() {
  editing.value = null
  testResult.value = null
  form.value = {
    name: '',
    host: '',
    port: 22,
    username: '',
    credential_type: 'key_path',
    credential_data: '',
    credential_passphrase: '',
    labels: '',
    team_id: auth.preferredTeamId || '',
  }
  showDialog.value = true
}

function openEdit(h: Host) {
  editing.value = h
  testResult.value = null
  form.value = {
    name: h.name,
    host: h.host,
    port: h.port,
    username: h.username,
    credential_type: h.credential_type,
    credential_data: '',       // masked
    credential_passphrase: '', // masked
    labels: (h.labels || []).join(', '),
    team_id: h.team_id || '',
  }
  showDialog.value = true
}

async function handleSave() {
  saving.value = true
  try {
    const payload: any = {
      name: form.value.name.trim(),
      host: form.value.host.trim(),
      port: Number(form.value.port) || 22,
      username: form.value.username.trim(),
      credential_type: form.value.credential_type,
      labels: form.value.labels
        ? form.value.labels.split(',').map((s: string) => s.trim()).filter(Boolean)
        : [],
      team_id: form.value.team_id || null,
    }
    const credData = form.value.credential_data.trim()
    if (form.value.credential_type === 'agent') {
      payload.credential_data = ''
      payload.credential_passphrase = ''
    } else {
      if (credData) {
        payload.credential_data = credData
      }
      const passphrase = form.value.credential_passphrase.trim()
      if (passphrase) {
        payload.credential_passphrase = passphrase
      }
    }
    if (editing.value) {
      payload.team_id = form.value.team_id || ''
      await hostsApi.update(editing.value.id, payload)
    } else {
      if (
        form.value.credential_type !== 'agent' &&
        !form.value.credential_data.trim()
      ) {
        alert(
          form.value.credential_type === 'key_path'
            ? 'Private key is required'
            : 'Password is required',
        )
        return
      }
      await hostsApi.create(payload)
    }
    showDialog.value = false
    await loadHosts()
  } finally {
    saving.value = false
  }
}

async function handleTest() {
  testResult.value = null
  if (!form.value.host.trim() || !form.value.username.trim()) {
    testResult.value = { ok: false, message: 'Address and username are required' }
    return
  }
  if (
    form.value.credential_type !== 'agent' &&
    !form.value.credential_data.trim() &&
    !(editing.value?.credential_stored)
  ) {
    testResult.value = {
      ok: false,
      message:
        form.value.credential_type === 'key_path'
          ? 'Private key is required to test'
          : 'Password is required to test',
    }
    return
  }

  testing.value = true
  try {
    const payload: Parameters<typeof hostsApi.test>[0] = {
      host: form.value.host.trim(),
      port: Number(form.value.port) || 22,
      username: form.value.username.trim(),
      credential_type: form.value.credential_type,
    }
    const credData = form.value.credential_data.trim()
    if (credData) payload.credential_data = credData
    const passphrase = form.value.credential_passphrase.trim()
    if (passphrase) payload.credential_passphrase = passphrase
    if (editing.value?.id) payload.host_id = editing.value.id

    const result = await hostsApi.test(payload)
    testResult.value = { ok: result.ok, message: result.message }
  } catch (e: any) {
    testResult.value = {
      ok: false,
      message: e.response?.data?.message || e.message || 'Test request failed',
    }
  } finally {
    testing.value = false
  }
}

async function handleDelete(h: Host) {
  if (!confirm(`Delete host "${h.name}"? This cannot be undone.`)) return
  await hostsApi.delete(h.id)
  await loadHosts()
}

async function loadHosts() {
  try {
    hosts.value = await hostsApi.list()
  } catch {
    // ignore
  } finally {
    loading.value = false
  }
}

function teamLabel(teamId: string | null | undefined): string {
  if (!teamId) return 'Shared (admin)'
  const t = availableTeams.value.find(x => x.id === teamId)
  return t ? `${t.name} (${t.slug})` : teamId
}

onMounted(async () => {
  try {
    availableTeams.value = await teamsApi.list()
  } catch { /* ignore */ }
  await loadHosts()
})
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      title="Hosts"
      description="SSH targets for pipeline steps. Reference by name or labels (uses: ssh + host / host_labels). Auth: SSH Key / Password / Agent — verify with Test connection."
    >
      <template #actions>
        <UiButton size="sm" @click="openCreate"><Plus class="h-4 w-4" /> Add Host</UiButton>
      </template>
    </PageHeader>

    <!-- Table -->
    <div v-if="loading" class="text-center py-12 text-muted-foreground text-sm">Loading…</div>
    <div v-else class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
      <!-- Filter bar -->
      <div class="px-5 py-3 border-b border-border/30 flex items-center gap-3 flex-wrap">
        <input
          v-model="searchQuery"
          type="text"
          placeholder="Search hosts…"
          class="w-full max-w-44 rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50 focus:ring-1 focus:ring-primary/10"
        />
        <select v-model="filterCredentialType" class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50">
          <option value="">All auth types</option>
          <option value="key_path">SSH Key</option>
          <option value="password">Password</option>
          <option value="agent">SSH Agent</option>
        </select>
        <span v-if="filteredHosts.length !== hosts.length" class="text-[10px] text-muted-foreground ml-auto">
          {{ filteredHosts.length }} of {{ hosts.length }}
        </span>
      </div>
      <div v-if="filteredHosts.length > 0" class="overflow-x-auto">
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
                <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('host')">
                  Host
                  <span class="font-mono text-xs">{{ sortIndicator('host') || '↕' }}</span>
                </button>
              </th>
              <th class="text-left px-5 py-2.5">
                <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('port')">
                  Port
                  <span class="font-mono text-xs">{{ sortIndicator('port') || '↕' }}</span>
                </button>
              </th>
              <th class="text-left px-5 py-2.5">
                <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('username')">
                  User
                  <span class="font-mono text-xs">{{ sortIndicator('username') || '↕' }}</span>
                </button>
              </th>
              <th class="text-left px-5 py-2.5">
                <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('credential_type')">
                  Auth
                  <span class="font-mono text-xs">{{ sortIndicator('credential_type') || '↕' }}</span>
                </button>
              </th>
              <th class="text-left px-5 py-2.5">
                <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('team')">
                  Team
                  <span class="font-mono text-xs">{{ sortIndicator('team') || '↕' }}</span>
                </button>
              </th>
              <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Labels</th>
              <th class="text-right px-5 py-2.5 w-20" />
            </tr>
          </thead>
          <tbody class="divide-y divide-border/20">
            <tr v-for="h in pageItems" :key="h.id" class="hover:bg-muted/5 transition-colors">
              <td class="px-5 py-3 font-semibold whitespace-nowrap">{{ h.name }}</td>
              <td class="px-5 py-3 font-mono text-xs text-muted-foreground whitespace-nowrap">{{ h.host }}</td>
              <td class="px-5 py-3 font-mono text-xs text-muted-foreground whitespace-nowrap">{{ h.port }}</td>
              <td class="px-5 py-3 whitespace-nowrap">{{ h.username }}</td>
              <td class="px-5 py-3 whitespace-nowrap">
                <span class="inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide bg-slate-500/10 text-slate-500">{{ h.credential_type }}</span>
              </td>
              <td class="px-5 py-3 whitespace-nowrap">
                <span
                  class="inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium tracking-wide"
                  :class="h.team_id ? 'bg-primary/10 text-primary' : 'bg-muted/60 text-muted-foreground'"
                >{{ teamLabel(h.team_id) }}</span>
              </td>
              <td class="px-5 py-3">
                <div class="flex flex-wrap gap-1">
                  <span v-for="l in h.labels" :key="l" class="mono-chip text-[10px]">{{ l }}</span>
                  <span v-if="!h.labels?.length" class="text-muted-foreground/40 text-xs">—</span>
                </div>
              </td>
              <td class="px-5 py-3 text-right whitespace-nowrap">
                <div class="flex items-center justify-end gap-1">
                  <UiButton variant="ghost" size="sm" @click="openEdit(h)" title="Edit"><Pencil class="h-3.5 w-3.5" /></UiButton>
                  <UiButton variant="ghost" size="sm" @click="handleDelete(h)" title="Delete"><Trash2 class="h-3.5 w-3.5 text-red-500" /></UiButton>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
        <TablePagination v-model:page="page" v-model:pageSize="pageSize" :total="total" :page-count="pageCount" :range-label="rangeLabel" />
      </div>
      <div v-else class="flex flex-col items-center justify-center py-16 text-muted-foreground">
        <Server class="h-10 w-10 mb-3 text-muted-foreground/20" />
        <p class="text-sm">{{ hosts.length === 0 ? 'No hosts configured yet' : 'No hosts in the selected team' }}</p>
        <p class="text-xs mt-1 text-muted-foreground/60">
          Reference hosts in SSH steps with <code class="mono-chip text-[10px]">host: name</code>
          or <code class="mono-chip text-[10px]">host_labels: [tag]</code>.
        </p>
      </div>
    </div>

    <!-- Dialog overlay -->
    <Teleport to="body">
      <div v-if="showDialog" class="fixed inset-0 z-50 flex items-start justify-center overflow-y-auto py-8">
        <div class="fixed inset-0 bg-black/30 backdrop-blur-sm" @click="showDialog = false" />
        <div class="relative z-10 w-full max-w-lg mx-4 my-auto rounded-2xl border border-border/50 bg-card shadow-2xl flex flex-col max-h-[min(85vh,calc(100vh-4rem))]">
          <h3 class="shrink-0 text-[15px] font-semibold px-6 pt-6 pb-4">{{ editing ? 'Edit Host' : 'New Host' }}</h3>
          <div class="flex-1 min-h-0 overflow-y-auto px-6 space-y-4">
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Name <span class="text-red-400">*</span></label>
              <input v-model="form.name" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20" placeholder="e.g. prod-server-01" />
            </div>
            <div class="grid grid-cols-2 gap-3">
              <div>
                <label class="block text-[12px] font-medium text-foreground mb-1">Address <span class="text-red-400">*</span></label>
                <input v-model="form.host" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20" placeholder="10.0.0.1" />
              </div>
              <div>
                <label class="block text-[12px] font-medium text-foreground mb-1">Port</label>
                <input v-model.number="form.port" type="number" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20" placeholder="22" />
              </div>
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Username <span class="text-red-400">*</span></label>
              <input v-model="form.username" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20" placeholder="root" />
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Auth Type</label>
              <div class="relative">
                <select v-model="form.credential_type" class="w-full appearance-none rounded-lg border border-border/60 bg-background px-3 py-2 pr-8 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20 cursor-pointer">
                  <option value="key_path">SSH Key</option>
                  <option value="vault">Password</option>
                  <option value="agent">SSH Agent</option>
                </select>
                <svg class="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground/60" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <polyline points="6 9 12 15 18 9" />
                </svg>
              </div>
            </div>
            <!-- SSH Agent: no credential stored in Manager -->
            <div
              v-if="form.credential_type === 'agent'"
              class="rounded-lg border border-sky-500/20 bg-sky-500/5 px-3 py-2.5 text-[12px] text-sky-700 dark:text-sky-300 leading-relaxed"
            >
              Uses the <span class="font-medium">runner's</span> SSH agent (<code class="font-mono text-[11px]">SSH_AUTH_SOCK</code>).
              No key or password is stored in the Manager. On the runner host, load keys with
              <code class="font-mono text-[11px]">ssh-add</code> before jobs run.
            </div>
            <!-- Edit mode: credential exists warning -->
            <div
              v-else-if="editing?.credential_stored"
              class="rounded-lg border border-amber-500/20 bg-amber-500/5 px-3 py-2.5 text-[12px] text-amber-600 dark:text-amber-400 leading-relaxed"
            >
              <span class="font-medium">{{ editing.credential_type === 'key_path' ? 'Private key' : 'Password' }} already stored.</span>
              Leave the field below empty to keep the existing credential, or paste a new value to replace it.
            </div>
            <div v-if="form.credential_type !== 'agent'">
              <label class="block text-[12px] font-medium text-foreground mb-1">
                {{ form.credential_type === 'key_path' ? 'Private Key' : 'Password' }}
                <span v-if="!editing?.credential_stored" class="text-red-500">*</span>
                <span class="text-muted-foreground/60 font-normal">
                  (paste directly — stored encrypted in DB)
                </span>
              </label>
              <textarea
                v-model="form.credential_data"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 font-mono text-xs outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20"
                rows="6"
                :placeholder="editing?.credential_stored ? 'Leave blank to keep existing credential' : (form.credential_type === 'key_path' ? '-----BEGIN OPENSSH PRIVATE KEY-----\n...\n-----END OPENSSH PRIVATE KEY-----' : 'Enter password')"
              ></textarea>
            </div>
            <!-- Key passphrase (only for SSH Key) -->
            <div v-if="form.credential_type === 'key_path'">
              <label class="block text-[12px] font-medium text-foreground mb-1">
                Key Passphrase
                <span class="text-muted-foreground/60 font-normal">
                  (optional — leave blank if key has no passphrase)
                </span>
              </label>
              <input
                v-model="form.credential_passphrase"
                type="password"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20"
                :placeholder="editing?.credential_stored ? 'Leave blank to keep existing' : 'Key passphrase (if any)'"
              />
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Team</label>
              <div class="relative">
                <select v-model="form.team_id" class="w-full appearance-none rounded-lg border border-border/60 bg-background px-3 py-2 pr-8 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20 cursor-pointer">
                  <option value="">Shared (admin)</option>
                  <option v-for="t in availableTeams" :key="t.id" :value="t.id">
                    {{ t.name }} ({{ t.slug }})
                  </option>
                </select>
                <svg class="pointer-events-none absolute right-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground/60" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                  <polyline points="6 9 12 15 18 9" />
                </svg>
              </div>
              <p class="mt-1 text-[11px] text-muted-foreground">Team-scoped hosts are visible to team members. Shared hosts require admin access.</p>
            </div>
            <div class="pb-1">
              <label class="block text-[12px] font-medium text-foreground mb-1">Labels <span class="text-muted-foreground/60 font-normal">(comma-separated)</span></label>
              <input v-model="form.labels" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20" placeholder="linux, x64, prod" />
            </div>
          </div>
          <div class="shrink-0 px-6 py-5 border-t border-border/30 space-y-3">
            <p
              v-if="testResult"
              class="text-[12px] leading-relaxed whitespace-pre-wrap rounded-lg px-3 py-2"
              :class="testResult.ok
                ? 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-300 border border-emerald-500/20'
                : 'bg-red-500/10 text-red-700 dark:text-red-300 border border-red-500/20'"
            >{{ testResult.message }}</p>
            <div class="flex items-center justify-between gap-3">
              <UiButton
                variant="outline"
                size="sm"
                :disabled="saving || testing || !form.host.trim() || !form.username.trim()"
                @click="handleTest"
              >
                <Loader2 v-if="testing" class="h-3.5 w-3.5 animate-spin" />
                {{ testing ? 'Testing…' : 'Test connection' }}
              </UiButton>
              <div class="flex items-center gap-3">
                <UiButton variant="ghost" size="sm" @click="showDialog = false" :disabled="saving || testing">Cancel</UiButton>
                <UiButton size="sm" @click="handleSave" :disabled="saving || testing">{{ saving ? 'Saving…' : (editing ? 'Update' : 'Create') }}</UiButton>
              </div>
            </div>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>
