<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import TablePagination from '@/components/ui/TablePagination.vue'
import { Plus, Pencil, Trash2, RefreshCw, PlugZap, GitBranch, Cable, ExternalLink } from '@lucide/vue'
import { githubSyncApi, type GitHubConnection, type GitHubRepo, type GitHubRepoBind } from '@/api/githubSync'
import { useClientSort } from '@/composables/useClientSort'
import { useClientPagination } from '@/composables/useClientPagination'
import { formatLocalDateTime } from '@/utils/time'
import { autoFetchStatus } from '@/utils/githubAutoFetch'

const connections = ref<GitHubConnection[]>([])
const repos = ref<GitHubRepo[]>([])
const loading = ref(true)

// ── Connections search / sort / pagination ──
const connSearch = ref('')
const filteredConnections = computed(() => {
  const q = connSearch.value.trim().toLowerCase()
  if (!q) return connections.value
  return connections.value.filter((c) => [c.name, c.api_base_url].filter(Boolean).join(' ').toLowerCase().includes(q))
})
const { sorted: sortedConns, toggleSort: toggleConnSort, sortIndicator: connSortIndicator } = useClientSort(filteredConnections, {
  initialKey: 'name',
  initialDir: 'asc',
  getValue: (row, key) => {
    switch (key) {
      case 'name': return row.name
      case 'api_base_url': return row.api_base_url
      default: return ''
    }
  },
})
const connPagination = useClientPagination(sortedConns)
const connPageItems = computed(() => connPagination.pageItems.value)
const connPage = connPagination.page
const connPageSize = connPagination.pageSize
const connPageCount = connPagination.pageCount
const connTotal = connPagination.total
const connRangeLabel = connPagination.rangeLabel

// ── Repos search / sort / pagination ──
const repoSearch = ref('')
const filteredRepos = computed(() => {
  const q = repoSearch.value.trim().toLowerCase()
  if (!q) return repos.value
  return repos.value.filter((r) =>
    [r.connection_name, r.owner, r.repo, ...(r.binds ?? []).map((b) => b.name)]
      .filter(Boolean)
      .join(' ')
      .toLowerCase()
      .includes(q),
  )
})
const { sorted: sortedRepos, toggleSort: toggleRepoSort, sortIndicator: repoSortIndicator } = useClientSort(filteredRepos, {
  initialKey: 'owner',
  initialDir: 'asc',
  getValue: (row, key) => {
    switch (key) {
      case 'connection': return row.connection_name
      case 'repo': return `${row.owner}/${row.repo}`
      default: return ''
    }
  },
})
const repoPagination = useClientPagination(sortedRepos)
const repoPageItems = computed(() => repoPagination.pageItems.value)
const repoPage = repoPagination.page
const repoPageSize = repoPagination.pageSize
const repoPageCount = repoPagination.pageCount
const repoTotal = repoPagination.total
const repoRangeLabel = repoPagination.rangeLabel

function repoBindChipClass(b: GitHubRepoBind) {
  switch (b.last_status) {
    case 'ok':
      return 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-400 border-emerald-500/20'
    case 'failed':
      return 'bg-red-500/10 text-red-700 dark:text-red-400 border-red-500/20'
    default:
      return 'bg-amber-500/10 text-amber-700 dark:text-amber-400 border-amber-500/20'
  }
}

function repoBindTitle(b: GitHubRepoBind) {
  const kind = b.kind === 'secret' ? 'Secret' : 'Variable'
  const state =
    b.last_op === 'unbind' && b.last_status === 'failed'
      ? 'unbind failed'
      : b.last_status === 'pending'
        ? 'bound, not pushed yet'
        : b.last_status === 'ok'
          ? 'pushed'
          : 'push failed'
  return `${kind} ${b.name} — ${state}`
}

function connAutoFetch(c: GitHubConnection) {
  return autoFetchStatus(c.fetch_interval_minutes, c.fetch_owners)
}

// ── Connection dialog ──
const showConnDialog = ref(false)
const editingConn = ref<GitHubConnection | null>(null)
const connName = ref('')
const connBaseUrl = ref('')
const connToken = ref('')
const connFetchOwners = ref('')
const connFetchIntervalInput = ref('')
const connSaving = ref(false)

function openCreateConn() {
  editingConn.value = null
  connName.value = ''
  connBaseUrl.value = 'https://api.github.com'
  connToken.value = ''
  connFetchOwners.value = ''
  connFetchIntervalInput.value = ''
  showConnDialog.value = true
}

function openEditConn(c: GitHubConnection) {
  editingConn.value = c
  connName.value = c.name
  connBaseUrl.value = c.api_base_url
  connToken.value = ''
  connFetchOwners.value = c.fetch_owners || ''
  connFetchIntervalInput.value = c.fetch_interval_minutes == null ? '' : String(c.fetch_interval_minutes)
  showConnDialog.value = true
}

async function saveConn() {
  if (!connName.value.trim()) {
    alert('Name is required')
    return
  }
  if (!editingConn.value && !connToken.value.trim()) {
    alert('Token is required')
    return
  }
  // `type="number"` v-model yields null when the field is cleared, not "".
  const intervalRaw = connFetchIntervalInput.value
  const fetchInterval = intervalRaw == null || String(intervalRaw).trim() === '' ? null : Number(intervalRaw)
  if (fetchInterval !== null && (!Number.isInteger(fetchInterval) || fetchInterval <= 0)) {
    alert('Fetch interval must be a positive whole number, or leave empty to disable')
    return
  }
  connSaving.value = true
  try {
    const base = {
      name: connName.value.trim(),
      api_base_url: connBaseUrl.value.trim(),
      fetch_owners: connFetchOwners.value.trim(),
      fetch_interval_minutes: fetchInterval,
    }
    if (editingConn.value) {
      await githubSyncApi.updateConnection(editingConn.value.id, {
        ...base,
        token: connToken.value.trim(),
      })
    } else {
      await githubSyncApi.createConnection({
        ...base,
        token: connToken.value.trim(),
      })
    }
    showConnDialog.value = false
    await loadAll()
  } catch (e: any) {
    alert(e?.response?.data?.message || e?.message || 'Failed to save connection')
  } finally {
    connSaving.value = false
  }
}

async function deleteConn(c: GitHubConnection) {
  if (!confirm(`Delete connection "${c.name}"? Its registered repos and all bindings will be removed.`)) return
  try {
    await githubSyncApi.deleteConnection(c.id)
    await loadAll()
  } catch (e: any) {
    alert(e?.response?.data?.message || e?.message || 'Failed to delete connection')
  }
}

// ── Repo dialog ──
const showRepoDialog = ref(false)
const editingRepo = ref<GitHubRepo | null>(null)
const repoConnId = ref('')
const repoOwner = ref('')
const repoName = ref('')
const repoSaving = ref(false)

function openCreateRepo() {
  editingRepo.value = null
  repoConnId.value = connections.value[0]?.id || ''
  repoOwner.value = ''
  repoName.value = ''
  showRepoDialog.value = true
}

function openEditRepo(r: GitHubRepo) {
  editingRepo.value = r
  repoConnId.value = r.connection_id
  repoOwner.value = r.owner
  repoName.value = r.repo
  showRepoDialog.value = true
}

async function saveRepo() {
  if (!repoOwner.value.trim() || !repoName.value.trim()) {
    alert('Owner and repo are required')
    return
  }
  repoSaving.value = true
  try {
    if (editingRepo.value) {
      await githubSyncApi.updateRepo(editingRepo.value.id, {
        owner: repoOwner.value.trim(),
        repo: repoName.value.trim(),
      })
    } else {
      await githubSyncApi.createRepo({
        connection_id: repoConnId.value,
        owner: repoOwner.value.trim(),
        repo: repoName.value.trim(),
      })
    }
    showRepoDialog.value = false
    await loadAll()
  } catch (e: any) {
    alert(e?.response?.data?.message || e?.message || 'Failed to save repo')
  } finally {
    repoSaving.value = false
  }
}

async function deleteRepo(r: GitHubRepo) {
  if (!confirm(`Unregister ${r.owner}/${r.repo}? Existing bindings to it are removed.`)) return
  try {
    await githubSyncApi.deleteRepo(r.id)
    await loadAll()
  } catch (e: any) {
    alert(e?.response?.data?.message || e?.message || 'Failed to delete repo')
  }
}

// ── Fetch repos dialog ──
const showFetchDialog = ref(false)
const fetchConnId = ref('')
const fetchOwner = ref('')
const fetchBusy = ref(false)
const fetchResult = ref<{ added: number; existing: number } | null>(null)

function openFetch() {
  fetchConnId.value = connections.value[0]?.id || ''
  fetchOwner.value = ''
  fetchResult.value = null
  showFetchDialog.value = true
}

async function doFetch() {
  if (!fetchOwner.value.trim()) {
    alert('Owner (org or user) is required')
    return
  }
  fetchBusy.value = true
  fetchResult.value = null
  try {
    fetchResult.value = await githubSyncApi.fetchRepos({
      connection_id: fetchConnId.value,
      owner: fetchOwner.value.trim(),
    })
    await loadAll()
  } catch (e: any) {
    alert(e?.response?.data?.message || e?.message || 'Fetch failed')
  } finally {
    fetchBusy.value = false
  }
}

// ── Test connection dialog ──
const showTestDialog = ref(false)
const testConnId = ref('')
const testOwner = ref('')
const testRepo = ref('')
const testBusy = ref(false)
const testResult = ref<'ok' | 'failed' | null>(null)
const testMessage = ref('')

function openTest() {
  testConnId.value = connections.value[0]?.id || ''
  testOwner.value = ''
  testRepo.value = ''
  testResult.value = null
  testMessage.value = ''
  showTestDialog.value = true
}

async function doTest() {
  testBusy.value = true
  testResult.value = null
  testMessage.value = ''
  try {
    const res = await githubSyncApi.testConnection({
      connection_id: testConnId.value,
      owner: testOwner.value.trim() || undefined,
      repo: testRepo.value.trim() || undefined,
    })
    testResult.value = 'ok'
    testMessage.value = res.login
      ? `Connection OK — token authenticated as ${res.login}.`
      : 'Connection OK — token can read the repo.'
  } catch (e: any) {
    testResult.value = 'failed'
    testMessage.value = e?.response?.data?.message || e?.message || 'Connection failed'
  } finally {
    testBusy.value = false
  }
}

async function loadAll() {
  try {
    const [c, r] = await Promise.all([githubSyncApi.listConnections(), githubSyncApi.listRepos()])
    connections.value = c
    repos.value = r
  } catch {
    // ignore
  } finally {
    loading.value = false
  }
}

onMounted(loadAll)
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      title="GitHub Sync"
      description="Pre-record GitHub connections (PAT) and repos, then bind secrets / variables to repos and push their values into GitHub Actions."
    >
      <template #actions>
        <UiButton size="sm" variant="outline" @click="openTest"><PlugZap class="h-4 w-4" /> Test</UiButton>
        <UiButton size="sm" variant="outline" @click="openFetch"><RefreshCw class="h-4 w-4" /> Fetch Repos</UiButton>
        <UiButton size="sm" @click="openCreateRepo"><Plus class="h-4 w-4" /> Add Repo</UiButton>
        <UiButton size="sm" @click="openCreateConn"><Plus class="h-4 w-4" /> Add Connection</UiButton>
      </template>
    </PageHeader>

    <div v-if="loading" class="text-center py-12 text-muted-foreground text-sm">Loading…</div>

    <template v-else>
      <!-- Connections -->
      <section class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
        <div class="px-5 py-3.5 border-b border-border/30 flex items-center gap-2">
          <Cable class="h-4 w-4 text-muted-foreground" />
          <h2 class="text-sm font-semibold">Connections</h2>
          <p class="text-[11px] text-muted-foreground ml-2">PAT + API base for pushing secrets/variables</p>
        </div>
        <div class="px-5 py-3 border-b border-border/30 flex items-center gap-3">
          <input
            v-model="connSearch"
            type="text"
            placeholder="Search connections…"
            class="w-full max-w-56 rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50 focus:ring-1 focus:ring-primary/10"
          />
        </div>
        <div v-if="connections.length === 0" class="flex flex-col items-center justify-center py-12 text-muted-foreground">
          <GitBranch class="h-8 w-8 mb-2 text-muted-foreground/20" />
          <p class="text-sm">No connections yet</p>
        </div>
        <div v-else class="overflow-x-auto">
          <table class="w-full text-[13px]">
            <thead>
              <tr class="border-b border-border/30 bg-muted/10">
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleConnSort('name')">
                    Name <span class="font-mono text-xs">{{ connSortIndicator('name') || '↕' }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleConnSort('api_base_url')">
                    API Base URL <span class="font-mono text-xs">{{ connSortIndicator('api_base_url') || '↕' }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <span class="text-[11px] uppercase text-muted-foreground">Token</span>
                </th>
                <th class="text-left px-5 py-2.5">
                  <span class="text-[11px] uppercase text-muted-foreground">Auto-fetch</span>
                </th>
                <th class="text-left px-5 py-2.5"><span class="text-[11px] uppercase text-muted-foreground">Updated</span></th>
                <th class="text-right px-5 py-2.5 w-24" />
              </tr>
            </thead>
            <tbody class="divide-y divide-border/20">
              <tr v-for="c in connPageItems" :key="c.id" class="hover:bg-muted/5 transition-colors">
                <td class="px-5 py-3 font-medium whitespace-nowrap">{{ c.name }}</td>
                <td class="px-5 py-3 text-xs text-muted-foreground whitespace-nowrap">{{ c.api_base_url }}</td>
                <td class="px-5 py-3">
                  <span v-if="c.has_token" class="inline-flex items-center rounded-md bg-emerald-500/10 px-1.5 py-0.5 text-[10px] font-medium text-emerald-700 dark:text-emerald-400">Set</span>
                  <span v-else class="inline-flex items-center rounded-md bg-amber-500/10 px-1.5 py-0.5 text-[10px] font-medium text-amber-700 dark:text-amber-400">Missing</span>
                </td>
                <td class="px-5 py-3 text-xs">
                  <template v-if="connAutoFetch(c).badge != null">
                    <span class="inline-flex items-center rounded-md bg-sky-500/10 px-1.5 py-0.5 text-[10px] font-medium text-sky-700 dark:text-sky-400">{{ connAutoFetch(c).badge }}</span>
                    <span
                      class="text-[10px] ml-1"
                      :class="connAutoFetch(c).warn ? 'text-amber-600 dark:text-amber-400' : 'text-muted-foreground'"
                    >{{ connAutoFetch(c).detail }}</span>
                  </template>
                  <span v-else class="text-[10px] text-muted-foreground/60">Off</span>
                </td>
                <td class="px-5 py-3 text-xs text-muted-foreground whitespace-nowrap">{{ formatLocalDateTime(c.updated_at) }}</td>
                <td class="px-5 py-3 text-right whitespace-nowrap">
                  <div class="flex items-center justify-end gap-1">
                    <UiButton variant="ghost" size="sm" @click="openTest" title="Test connection"><PlugZap class="h-3.5 w-3.5" /></UiButton>
                    <UiButton variant="ghost" size="sm" @click="openEditConn(c)" title="Edit"><Pencil class="h-3.5 w-3.5" /></UiButton>
                    <UiButton variant="ghost" size="sm" @click="deleteConn(c)" title="Delete"><Trash2 class="h-3.5 w-3.5 text-red-500" /></UiButton>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <TablePagination
          v-if="connections.length > 0"
          v-model:page="connPage"
          v-model:pageSize="connPageSize"
          :total="connTotal"
          :page-count="connPageCount"
          :range-label="connRangeLabel"
        />
      </section>

      <!-- Repos -->
      <section class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
        <div class="px-5 py-3.5 border-b border-border/30 flex items-center gap-2">
          <GitBranch class="h-4 w-4 text-muted-foreground" />
          <h2 class="text-sm font-semibold">Repositories</h2>
          <p class="text-[11px] text-muted-foreground ml-2">Reusable targets for secret/variable bindings</p>
        </div>
        <div class="px-5 py-3 border-b border-border/30 flex items-center gap-3">
          <input
            v-model="repoSearch"
            type="text"
            placeholder="Search repos…"
            class="w-full max-w-56 rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50 focus:ring-1 focus:ring-primary/10"
          />
        </div>
        <div v-if="repos.length === 0" class="flex flex-col items-center justify-center py-12 text-muted-foreground">
          <GitBranch class="h-8 w-8 mb-2 text-muted-foreground/20" />
          <p class="text-sm">No repos registered</p>
          <p class="text-xs mt-1 text-muted-foreground/60">
            Use <span class="font-medium text-foreground">Fetch Repos</span> to import an org/user's repos, or add manually.
          </p>
        </div>
        <div v-else class="overflow-x-auto">
          <table class="w-full text-[13px]">
            <thead>
              <tr class="border-b border-border/30 bg-muted/10">
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleRepoSort('connection')">
                    Connection <span class="font-mono text-xs">{{ repoSortIndicator('connection') || '↕' }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleRepoSort('repo')">
                    Repo <span class="font-mono text-xs">{{ repoSortIndicator('repo') || '↕' }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5"><span class="text-[11px] uppercase text-muted-foreground">Bound</span></th>
                <th class="text-left px-5 py-2.5"><span class="text-[11px] uppercase text-muted-foreground">Updated</span></th>
                <th class="text-right px-5 py-2.5 w-20" />
              </tr>
            </thead>
            <tbody class="divide-y divide-border/20">
              <tr v-for="r in repoPageItems" :key="r.id" class="hover:bg-muted/5 transition-colors">
                <td class="px-5 py-3 text-xs text-muted-foreground whitespace-nowrap">{{ r.connection_name }}</td>
                <td class="px-5 py-3 whitespace-nowrap">
                  <a
                    :href="r.url"
                    target="_blank"
                    rel="noopener noreferrer"
                    class="inline-flex items-center gap-1.5 font-mono text-[13px] font-semibold text-foreground hover:text-primary transition-colors"
                    :title="`Open ${r.owner}/${r.repo} on GitHub`"
                  >
                    {{ r.owner }}/{{ r.repo }}
                    <ExternalLink class="h-3 w-3 text-muted-foreground/50" />
                  </a>
                </td>
                <td class="px-5 py-3">
                  <div v-if="r.binds && r.binds.length > 0" class="flex flex-wrap gap-1">
                    <span
                      v-for="b in r.binds"
                      :key="b.id"
                      :title="repoBindTitle(b)"
                      class="inline-flex items-center gap-1 rounded-md border px-1.5 py-0.5 font-mono text-[10px] font-medium"
                      :class="repoBindChipClass(b)"
                    >
                      <span class="font-sans font-semibold opacity-70">{{ b.kind === 'secret' ? 'S' : 'V' }}</span>
                      {{ b.name }}
                    </span>
                  </div>
                  <span v-else class="text-[11px] text-muted-foreground/50">—</span>
                </td>
                <td class="px-5 py-3 text-xs text-muted-foreground whitespace-nowrap">{{ formatLocalDateTime(r.updated_at) }}</td>
                <td class="px-5 py-3 text-right whitespace-nowrap">
                  <div class="flex items-center justify-end gap-1">
                    <UiButton variant="ghost" size="sm" @click="openEditRepo(r)" title="Edit"><Pencil class="h-3.5 w-3.5" /></UiButton>
                    <UiButton variant="ghost" size="sm" @click="deleteRepo(r)" title="Delete"><Trash2 class="h-3.5 w-3.5 text-red-500" /></UiButton>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <TablePagination
          v-if="repos.length > 0"
          v-model:page="repoPage"
          v-model:pageSize="repoPageSize"
          :total="repoTotal"
          :page-count="repoPageCount"
          :range-label="repoRangeLabel"
        />
      </section>
    </template>

    <!-- Connection dialog -->
    <Teleport to="body">
      <div v-if="showConnDialog" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="showConnDialog = false" />
        <div class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6">
          <h3 class="text-[15px] font-semibold mb-5">{{ editingConn ? 'Edit Connection' : 'New Connection' }}</h3>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Name <span class="text-red-500">*</span></label>
              <input
                v-model="connName"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20"
                placeholder="e.g. github-com-free-org"
              />
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">API Base URL</label>
              <input
                v-model="connBaseUrl"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20"
                placeholder="https://api.github.com"
              />
              <p class="text-[10px] text-muted-foreground mt-1">Defaults to api.github.com; change for GitHub Enterprise.</p>
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">
                Token <span class="text-red-500">*</span>
                <span v-if="editingConn" class="text-muted-foreground font-normal ml-1">(leave empty to keep current)</span>
              </label>
              <input
                v-model="connToken"
                type="password"
                autocomplete="new-password"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20"
                placeholder="ghp_… or github_pat_…"
              />
              <p class="text-[10px] text-muted-foreground mt-1 leading-relaxed">
                Needs repo access + <strong>Actions</strong> read/write (fine-grained) or
                <code class="mono-chip text-[10px]">repo</code> scope. Encrypted at rest, never returned.
              </p>
            </div>
            <div class="rounded-xl border border-border/40 bg-muted/20 p-3 space-y-3">
              <p class="text-[11px] font-semibold text-muted-foreground uppercase tracking-wide">Auto-fetch (optional)</p>
              <div>
                <label class="block text-[12px] font-medium text-foreground mb-1">Fetch Owners</label>
                <textarea
                  v-model="connFetchOwners"
                  rows="2"
                  class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20 resize-none"
                  placeholder="org1, org2 — comma or newline separated"
                />
                <p class="text-[10px] text-muted-foreground mt-1">Owners pulled on the interval below; leave empty to disable. Auto-fetch only runs when both an interval and at least one owner are set.</p>
              </div>
              <div>
                <label class="block text-[12px] font-medium text-foreground mb-1">Fetch Interval (minutes)</label>
                <input
                  v-model="connFetchIntervalInput"
                  type="number"
                  min="1"
                  class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20"
                  placeholder="e.g. 30"
                />
                <p class="text-[10px] text-muted-foreground mt-1">Poll GitHub every N minutes; empty disables auto-fetch. Requires at least one Fetch Owner to take effect.</p>
              </div>
            </div>
          </div>
          <div class="flex items-center justify-end gap-3 mt-6 pt-5 border-t border-border/30">
            <UiButton variant="ghost" size="sm" @click="showConnDialog = false" :disabled="connSaving">Cancel</UiButton>
            <UiButton size="sm" @click="saveConn" :disabled="connSaving">{{ connSaving ? 'Saving…' : (editingConn ? 'Update' : 'Create') }}</UiButton>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- Repo dialog -->
    <Teleport to="body">
      <div v-if="showRepoDialog" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="showRepoDialog = false" />
        <div class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6">
          <h3 class="text-[15px] font-semibold mb-5">{{ editingRepo ? 'Edit Repo' : 'Register Repo' }}</h3>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Connection <span class="text-red-500">*</span></label>
              <select
                v-model="repoConnId"
                :disabled="!!editingRepo"
                class="w-full appearance-none rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20 cursor-pointer disabled:bg-muted/30 disabled:cursor-not-allowed"
              >
                <option disabled value="">Select a connection…</option>
                <option v-for="c in connections" :key="c.id" :value="c.id">{{ c.name }}</option>
              </select>
              <p v-if="connections.length === 0" class="text-[10px] text-red-500 mt-1">Create a connection first.</p>
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Owner <span class="text-red-500">*</span></label>
              <input
                v-model="repoOwner"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20"
                placeholder="org-name or username"
              />
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Repo <span class="text-red-500">*</span></label>
              <input
                v-model="repoName"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20"
                placeholder="my-service"
              />
            </div>
          </div>
          <div class="flex items-center justify-end gap-3 mt-6 pt-5 border-t border-border/30">
            <UiButton variant="ghost" size="sm" @click="showRepoDialog = false" :disabled="repoSaving">Cancel</UiButton>
            <UiButton size="sm" @click="saveRepo" :disabled="repoSaving">{{ repoSaving ? 'Saving…' : (editingRepo ? 'Update' : 'Register') }}</UiButton>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- Fetch repos dialog -->
    <Teleport to="body">
      <div v-if="showFetchDialog" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="showFetchDialog = false" />
        <div class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6">
          <h3 class="text-[15px] font-semibold mb-5">Fetch Repositories</h3>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Connection <span class="text-red-500">*</span></label>
              <select
                v-model="fetchConnId"
                class="w-full appearance-none rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20 cursor-pointer"
              >
                <option v-for="c in connections" :key="c.id" :value="c.id">{{ c.name }}</option>
              </select>
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Owner (org or user) <span class="text-red-500">*</span></label>
              <input
                v-model="fetchOwner"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20"
                placeholder="my-org"
              />
              <p class="text-[10px] text-muted-foreground mt-1 leading-relaxed">
                Imports repos the token can see (org first, user fallback). Existing repos are kept.
              </p>
            </div>
            <div v-if="fetchResult" class="rounded-lg bg-emerald-500/10 border border-emerald-500/20 px-3 py-2 text-[11px] text-emerald-700 dark:text-emerald-400">
              Imported {{ fetchResult.added }} new, {{ fetchResult.existing }} already registered.
            </div>
          </div>
          <div class="flex items-center justify-end gap-3 mt-6 pt-5 border-t border-border/30">
            <UiButton variant="ghost" size="sm" @click="showFetchDialog = false" :disabled="fetchBusy">Cancel</UiButton>
            <UiButton size="sm" @click="doFetch" :disabled="fetchBusy">
              <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': fetchBusy }" />
              {{ fetchBusy ? 'Fetching…' : 'Fetch' }}
            </UiButton>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- Test dialog -->
    <Teleport to="body">
      <div v-if="showTestDialog" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="showTestDialog = false" />
        <div class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6">
          <h3 class="text-[15px] font-semibold mb-5">Test Connection</h3>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Connection <span class="text-red-500">*</span></label>
              <select
                v-model="testConnId"
                class="w-full appearance-none rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20 cursor-pointer"
              >
                <option v-for="c in connections" :key="c.id" :value="c.id">{{ c.name }}</option>
              </select>
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Owner</label>
              <input
                v-model="testOwner"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20"
                placeholder="my-org"
              />
              <p class="text-[10px] text-muted-foreground mt-1">Optional — leave empty to just validate the token.</p>
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Repo</label>
              <input
                v-model="testRepo"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20"
                placeholder="(optional) verify a specific repo"
              />
            </div>
            <div v-if="testResult" class="rounded-lg px-3 py-2 text-[11px]" :class="testResult === 'ok' ? 'bg-emerald-500/10 border border-emerald-500/20 text-emerald-700 dark:text-emerald-400' : 'bg-red-500/10 border border-red-500/20 text-red-700 dark:text-red-400'">
              {{ testMessage }}
            </div>
          </div>
          <div class="flex items-center justify-end gap-3 mt-6 pt-5 border-t border-border/30">
            <UiButton variant="ghost" size="sm" @click="showTestDialog = false" :disabled="testBusy">Cancel</UiButton>
            <UiButton size="sm" @click="doTest" :disabled="testBusy">
              <PlugZap class="h-3.5 w-3.5" />
              {{ testBusy ? 'Testing…' : 'Test' }}
            </UiButton>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>
