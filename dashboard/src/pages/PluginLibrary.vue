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

const poolEnabled = computed(() => pool.value.filter((p) => p.enabled))
const poolStaged = computed(() => pool.value.filter((p) => !p.enabled))

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

      <!-- ── Pool: staged + enabled ── -->
      <section class="rounded-xl border border-border/50 bg-card overflow-hidden">
        <div class="px-5 py-4 border-b border-border/50">
          <h2 class="text-sm font-semibold">Pool</h2>
          <p class="mt-0.5 text-xs text-muted-foreground">
            Staged entries come from source syncs and must be enabled before
            <code class="font-mono text-[10px]">uses:</code> can reference them.
          </p>
        </div>
        <div class="divide-y divide-border/50">
          <div v-for="entry in [...poolEnabled, ...poolStaged]" :key="`${entry.namespace}/${entry.step_id}/${entry.version}`" class="flex items-start justify-between gap-3 px-5 py-3">
            <div class="min-w-0">
              <div class="flex items-center gap-2">
                <code class="font-mono text-xs">{{ entry.namespace }}/{{ entry.step_id }}@{{ entry.version }}</code>
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
              <p class="mt-0.5 truncate font-mono text-[11px] text-muted-foreground">
                sha256 {{ entry.sha256.slice(0, 16) }}… · {{ entry.source_name ?? entry.source_id }}
              </p>
            </div>
            <UiButton size="sm" variant="ghost" :disabled="busy === `${entry.namespace}/${entry.step_id}/${entry.version}`" @click="toggleEntry(entry)">
              {{ entry.enabled ? 'Disable' : 'Enable' }}
            </UiButton>
          </div>
          <p v-if="pool.length === 0" class="px-5 py-4 text-sm text-muted-foreground">
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
          <div v-for="(tpl, index) in localTemplates" :key="index" class="px-5 py-3">
            <code class="font-mono text-xs">{{ (tpl as Record<string, unknown>).id ?? 'unknown' }}</code>
            <span class="ml-2 text-[11px] text-muted-foreground">v{{ (tpl as Record<string, unknown>).version ?? '?' }}</span>
          </div>
          <p v-if="localTemplates.length === 0" class="px-5 py-4 text-sm text-muted-foreground">
            No local templates.
          </p>
        </div>
      </section>
    </div>
  </div>
</template>
