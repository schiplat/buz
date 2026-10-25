<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import UiButton from '@/components/ui/UiButton.vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import { stepSourcesApi, type PoolEntry } from '@/api/stepSources'
import { useAppDialog } from '@/composables/useAppDialog'

const dialog = useAppDialog()
const loading = ref(true)
const pool = ref<PoolEntry[]>([])
const busy = ref('')
const refreshing = ref('')

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


async function load() {
  loading.value = true
  try {
    const plt = await stepSourcesApi.listPool()
    pool.value = plt.items ?? []
  } catch (e) {
    await dialog.alert(`Failed to load the pool: ${e instanceof Error ? e.message : e}`)
  } finally {
    loading.value = false
  }
}

/** Re-fetch one plugin's metadata from its source (per-group refresh). */
async function refreshGroup(group: { namespace: string; stepId: string }) {
  refreshing.value = `${group.namespace}/${group.stepId}`
  try {
    await stepSourcesApi.refreshPlugin(group.namespace, group.stepId)
    await load()
  } catch (e) {
    await dialog.alert(`Refresh failed: ${e instanceof Error ? e.message : e}`)
  } finally {
    refreshing.value = ''
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

onMounted(load)
</script>

<template>
  <div class="space-y-6">
    <!-- ── Pool: per-plugin version lists ── -->
    <section class="rounded-xl border border-border/50 bg-card overflow-hidden">
      <div class="px-5 py-4 border-b border-border/50">
        <h2 class="text-sm font-semibold">Available plugins</h2>
        <p class="mt-0.5 text-xs text-muted-foreground">
          Every plugin you can reference with <code class="font-mono text-[10px]">uses:</code>, with its full version list. Staged versions must be enabled before
          <code class="font-mono text-[10px]">uses:</code> can reference them.
        </p>
      </div>
      <div class="divide-y divide-border/50">
        <div v-for="group in poolGroups" :key="`${group.namespace}/${group.stepId}`" class="px-5 py-4">
          <div class="flex items-center justify-between gap-3">
            <div class="flex min-w-0 items-center gap-2">
              <code class="font-mono text-xs font-semibold">{{ group.namespace }}/{{ group.stepId }}</code>
              <span
                v-if="group.updateAvailable"
                class="rounded-full bg-amber-500/10 px-2 py-0.5 text-[11px] text-amber-600 dark:text-amber-400"
              >update available</span>
            </div>
            <UiButton
              size="sm"
              variant="ghost"
              :disabled="refreshing === `${group.namespace}/${group.stepId}`"
              title="Re-fetch this plugin's versions from its source"
              @click="refreshGroup(group)"
            ><RefreshCw class="h-3.5 w-3.5" /> {{ refreshing === `${group.namespace}/${group.stepId}` ? 'Refreshing…' : 'Refresh' }}</UiButton>
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
  </div>
</template>
