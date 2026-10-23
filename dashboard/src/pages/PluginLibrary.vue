<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { RefreshCw, Trash2 } from '@lucide/vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import {
  stepSourcesApi,
  stepsApi,
  type PoolEntry,
  type StepSource,
} from '@/api/stepSources'
import { useAuthStore } from '@/stores/auth'
import { useRouter } from 'vue-router'
import { useAppDialog } from '@/composables/useAppDialog'

const auth = useAuthStore()
const router = useRouter()
const dialog = useAppDialog()

const loading = ref(true)
const sources = ref<StepSource[]>([])
const pool = ref<PoolEntry[]>([])
const localTemplates = ref<Array<Record<string, unknown>>>([])
const showAddSource = ref(false)
const newName = ref('')
const newUrl = ref('')
const newAutoEnable = ref(false)
const busy = ref('')

interface PoolGroup {
  namespace: string
  stepId: string
  sourceName: string
  /** All known versions, newest first. */
  versions: PoolEntry[]
  /** True when a staged version is newer than every enabled one. */
  updateAvailable: boolean
}

/** Per-plugin version lists: the pool grouped by namespace/step-id, versions
 *  newest first, with the "update available" verdict per group. */
const poolGroups = computed<PoolGroup[]>(() => {
  const groups = new Map<string, PoolGroup>()
  for (const entry of pool.value) {
    const key = `${entry.namespace}/${entry.step_id}`
    if (!groups.has(key)) {
      groups.set(key, {
        namespace: entry.namespace,
        stepId: entry.step_id,
        sourceName: entry.source_name ?? entry.source_id,
        versions: [],
        updateAvailable: false,
      })
    }
    groups.get(key)!.versions.push(entry)
  }
  for (const group of groups.values()) {
    group.versions.sort((a, b) => {
      const ta = versionTriple(a.version) ?? [0, 0, 0]
      const tb = versionTriple(b.version) ?? [0, 0, 0]
      return compareTriple(tb, ta) // newest first
    })
    const maxEnabled = group.versions
      .filter((v) => v.enabled)
      .map((v) => versionTriple(v.version))
      .filter((v): v is [number, number, number] => v !== null)
      .sort(compareTriple)
      .at(-1)
    const maxStaged = group.versions
      .filter((v) => !v.enabled)
      .map((v) => versionTriple(v.version))
      .filter((v): v is [number, number, number] => v !== null)
      .sort(compareTriple)
      .at(-1)
    group.updateAvailable = !!maxStaged && (!maxEnabled || compareTriple(maxStaged, maxEnabled) > 0)
  }
  return [...groups.values()].sort((a, b) =>
    `${a.namespace}/${a.stepId}`.localeCompare(`${b.namespace}/${b.stepId}`),
  )
})

interface LocalGroup {
  id: string
  /** All known versions, newest first. */
  versions: Array<Record<string, unknown>>
}

/** Local (local_dir) templates grouped per plugin id, versions newest first. */
const localGroups = computed<LocalGroup[]>(() => {
  const groups = new Map<string, LocalGroup>()
  for (const tpl of localTemplates.value) {
    const id = String(tpl.id ?? 'unknown')
    if (!groups.has(id)) groups.set(id, { id, versions: [] })
    groups.get(id)!.versions.push(tpl)
  }
  for (const group of groups.values()) {
    group.versions.sort((a, b) => {
      const ta = versionTriple(String(a.version ?? '')) ?? [0, 0, 0]
      const tb = versionTriple(String(b.version ?? '')) ?? [0, 0, 0]
      return compareTriple(tb, ta) // newest first
    })
  }
  return [...groups.values()].sort((a, b) => a.id.localeCompare(b.id))
})

function versionTriple(v: string): [number, number, number] | null {
  const parts = v.split('.').map(Number)
  if (parts.length === 0 || parts.some((n) => !Number.isFinite(n))) return null
  const [major = 0, minor = 0, patch = 0] = parts
  return [major, minor, patch]
}

function compareTriple(
  a: [number, number, number],
  b: [number, number, number],
): number {
  return a[0] - b[0] || a[1] - b[1] || a[2] - b[2]
}

async function load() {
  if (!auth.isPlatformAdmin) {
    router.replace('/dashboard')
    return
  }
  loading.value = true
  try {
    const [src, plt, local] = await Promise.all([
      stepSourcesApi.listSources(),
      stepSourcesApi.listPool(),
      stepsApi.listLocal().catch(() => ({ items: [], unparseable: [] })),
    ])
    sources.value = src.items ?? []
    pool.value = plt.items ?? []
    localTemplates.value = local.items ?? []
  } catch (e) {
    await dialog.alert(`Failed to load plugin library: ${e instanceof Error ? e.message : e}`)
  } finally {
    loading.value = false
  }
}

async function addSource() {
  if (!newName.value.trim() || !newUrl.value.trim()) return
  busy.value = 'add'
  try {
    const res = await stepSourcesApi.createSource(
      newName.value.trim(),
      newUrl.value.trim(),
      newAutoEnable.value,
    )
    if (!res.synced) {
      await dialog.alert(
        'Source saved, but the initial sync failed — check the sync error shown in the list.',
      )
    }
    showAddSource.value = false
    newName.value = ''
    newUrl.value = ''
    newAutoEnable.value = false
    await load()
  } catch (e) {
    await dialog.alert(`Failed to add source: ${e instanceof Error ? e.message : e}`)
  } finally {
    busy.value = ''
  }
}

async function syncSource(source: StepSource) {
  busy.value = `sync-${source.id}`
  try {
    const res = await stepSourcesApi.syncSource(source.id)
    if (!res.synced) {
      await dialog.alert(`Sync failed: ${res.error ?? 'unknown error'}`)
    }
    await load()
  } finally {
    busy.value = ''
  }
}

async function removeSource(source: StepSource) {
  const ok = await dialog.confirm(`Remove source "${source.name}" and its pool entries?`, {
    confirmLabel: 'Remove',
  })
  if (!ok) return
  busy.value = `del-${source.id}`
  try {
    await stepSourcesApi.deleteSource(source.id)
    await load()
  } finally {
    busy.value = ''
  }
}

async function toggleEntry(entry: PoolEntry) {
  busy.value = `${entry.namespace}/${entry.step_id}/${entry.version}`
  try {
    await stepSourcesApi.togglePoolEntry(
      entry.namespace,
      entry.step_id,
      entry.version,
      !entry.enabled,
    )
    await load()
  } finally {
    busy.value = ''
  }
}

function syncState(source: StepSource): string {
  if (source.last_sync_error) return `sync error: ${source.last_sync_error}`
  if (source.last_synced_at) return `synced ${source.last_synced_at}`
  return 'never synced'
}

onMounted(load)
</script>

<template>
  <div>
    <PageHeader
      title="Plugin library"
      description="Templates pipeline authors may reference with uses:. Official and community sources share the same pipeline — curating the pool is the only gate."
    />

    <div v-if="loading" class="text-sm text-muted-foreground">Loading…</div>
    <div v-else class="space-y-6">
      <!-- ── Sources ── -->
      <section class="rounded-xl border border-border/50 bg-card overflow-hidden">
        <div class="flex items-center justify-between px-5 py-4 border-b border-border/50">
          <h2 class="text-sm font-semibold">Sources</h2>
          <UiButton size="sm" @click="showAddSource = !showAddSource">
            {{ showAddSource ? 'Cancel' : 'Add source' }}
          </UiButton>
        </div>

        <div v-if="showAddSource" class="space-y-3 border-b border-border/50 bg-muted/20 px-5 py-4">
          <div class="grid gap-3 sm:grid-cols-2">
            <div>
              <label class="type-label block mb-1.5">Name</label>
              <input v-model="newName" type="text" placeholder="e.g. alice" class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm" />
            </div>
            <div>
              <label class="type-label block mb-1.5">Index URL</label>
              <input v-model="newUrl" type="url" placeholder="https://…/index.json" class="w-full rounded-xl border border-border bg-input px-3 py-2 font-mono text-xs" />
            </div>
          </div>
          <label class="flex items-center gap-2 text-xs cursor-pointer">
            <input v-model="newAutoEnable" type="checkbox" class="rounded-sm border-border" />
            <span>Auto-enable entries (trusted source — skips per-template curation)</span>
          </label>
          <UiButton size="sm" :disabled="busy === 'add'" @click="addSource">
            {{ busy === 'add' ? 'Adding…' : 'Add & sync' }}
          </UiButton>
        </div>

        <div class="divide-y divide-border/50">
          <div v-for="source in sources" :key="source.id" class="px-5 py-4">
            <div class="flex items-start justify-between gap-3">
              <div class="min-w-0">
                <div class="flex items-center gap-2">
                  <span class="text-sm font-medium">{{ source.name }}</span>
                  <span
                    v-if="source.auto_enable"
                    class="rounded-full bg-primary/10 px-2 py-0.5 text-[11px] text-primary"
                  >auto-enable</span>
                </div>
                <p class="mt-0.5 truncate font-mono text-[11px] text-muted-foreground" :title="source.url">{{ source.url }}</p>
                <p
                  class="mt-0.5 text-[11px]"
                  :class="source.last_sync_error ? 'text-red-500' : 'text-muted-foreground/70'"
                >{{ syncState(source) }}</p>
              </div>
              <div class="flex shrink-0 items-center gap-1.5">
                <UiButton size="sm" variant="ghost" :disabled="busy === `sync-${source.id}`" @click="syncSource(source)">
                  <RefreshCw class="h-3.5 w-3.5" /> Sync
                </UiButton>
                <UiButton size="sm" variant="ghost" :disabled="busy === `del-${source.id}`" @click="removeSource(source)">
                  <Trash2 class="h-3.5 w-3.5" />
                </UiButton>
              </div>
            </div>
          </div>
          <p v-if="sources.length === 0" class="px-5 py-4 text-sm text-muted-foreground">
            No sources configured. Official templates resolve from the local step directory.
          </p>
        </div>
      </section>

      <!-- ── Pool: per-plugin version lists ── -->
      <section class="rounded-xl border border-border/50 bg-card overflow-hidden">
        <div class="px-5 py-4 border-b border-border/50">
          <h2 class="text-sm font-semibold">Pool</h2>
          <p class="mt-0.5 text-xs text-muted-foreground">
            Every plugin with its full version list. Staged versions must be enabled before
            <code class="font-mono text-[10px]">uses:</code> can reference them.
          </p>
        </div>
        <div class="divide-y divide-border/50">
          <div v-for="group in poolGroups" :key="`${group.namespace}/${group.stepId}`" class="px-5 py-4">
            <div class="flex items-center justify-between gap-3">
              <code class="font-mono text-xs font-semibold">{{ group.namespace }}/{{ group.stepId }}</code>
              <span
                v-if="group.updateAvailable"
                class="rounded-full bg-amber-500/10 px-2 py-0.5 text-[11px] text-amber-600 dark:text-amber-400"
              >update available</span>
            </div>
            <p class="mt-0.5 text-[11px] text-muted-foreground">source: {{ group.sourceName }}</p>
            <div class="mt-2 space-y-1.5">
              <div
                v-for="entry in group.versions"
                :key="`${entry.namespace}/${entry.step_id}/${entry.version}`"
                class="flex items-center justify-between gap-3 rounded-lg bg-muted/30 px-3 py-2"
              >
                <div class="flex min-w-0 items-center gap-2">
                  <code class="font-mono text-xs">v{{ entry.version }}</code>
                  <span
                    v-if="entry.enabled"
                    class="rounded-full bg-green-500/10 px-2 py-0.5 text-[11px] text-green-600 dark:text-green-400"
                  >enabled</span>
                  <span v-else class="rounded-full bg-muted px-2 py-0.5 text-[11px] text-muted-foreground">staged</span>
                  <span
                    v-if="entry.reviewed"
                    class="rounded-full bg-blue-500/10 px-2 py-0.5 text-[11px] text-blue-600 dark:text-blue-400"
                  >reviewed</span>
                </div>
                <div class="flex shrink-0 items-center gap-2">
                  <span class="hidden font-mono text-[10px] text-muted-foreground/70 sm:inline" :title="entry.sha256">
                    {{ entry.sha256.slice(0, 12) }}…
                  </span>
                  <UiButton size="sm" variant="ghost" :disabled="busy === `${entry.namespace}/${entry.step_id}/${entry.version}`" @click="toggleEntry(entry)">
                    {{ entry.enabled ? 'Disable' : 'Enable' }}
                  </UiButton>
                </div>
              </div>
            </div>
          </div>
          <p v-if="poolGroups.length === 0" class="px-5 py-4 text-sm text-muted-foreground">
            Pool is empty — sync a source to stage templates for curation.
          </p>
        </div>
      </section>

      <!-- ── Local templates (local_dir / official git-clone path) ── -->
      <section class="rounded-xl border border-border/50 bg-card overflow-hidden">
        <div class="px-5 py-4 border-b border-border/50">
          <h2 class="text-sm font-semibold">Local templates</h2>
          <p class="mt-0.5 text-xs text-muted-foreground">
            Served from the configured local step directory (git-ops / offline deployments). Always enabled.
          </p>
        </div>
        <div class="divide-y divide-border/50">
          <div v-for="group in localGroups" :key="group.id" class="px-5 py-3">
            <code class="font-mono text-xs font-semibold">{{ group.id }}</code>
            <div class="mt-1 flex flex-wrap items-center gap-1.5">
              <span
                v-for="tpl in group.versions"
                :key="String(tpl.version)"
                class="rounded-full bg-muted px-2 py-0.5 font-mono text-[11px]"
              >v{{ tpl.version ?? '?' }}</span>
            </div>
          </div>
          <p v-if="localGroups.length === 0" class="px-5 py-4 text-sm text-muted-foreground">
            No local templates.
          </p>
        </div>
      </section>
    </div>
  </div>
</template>
