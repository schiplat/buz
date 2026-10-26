<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { AlertTriangle, Check, RefreshCw } from '@lucide/vue'
import UiButton from '@/components/ui/UiButton.vue'
import TablePagination from '@/components/ui/TablePagination.vue'
import { stepSourcesApi, type PoolEntry } from '@/api/stepSources'
import { useAppDialog } from '@/composables/useAppDialog'
import { useClientPagination } from '@/composables/useClientPagination'

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

/** Group-level pagination: a refresh that only flips enable states must not
 *  throw the curator off their page — the reset key tracks the plugin set. */
const groupSetKey = computed(() =>
  poolGroups.value.map((g) => `${g.namespace}/${g.stepId}`).join('|'),
)
const { page, pageSize, pageCount, total, pageItems, rangeLabel } = useClientPagination(
  poolGroups,
  { resetKey: groupSetKey },
)

const isStuck = (entry: PoolEntry) => entry.buz_compatible === false
const entryKey = (entry: PoolEntry) => `${entry.namespace}/${entry.step_id}/${entry.version}`

/** Tooltip line: identity, state, compatibility, sha — the whole chip in one
 *  hover. */
function chipTitle(entry: PoolEntry): string {
  const parts = [`v${entry.version}`, entry.enabled ? 'enabled' : 'staged']
  if (entry.reviewed) parts.push('reviewed')
  if (isStuck(entry)) parts.push('incompatible with this Buz')
  else if (entry.buz_compatible === null) parts.push('compatibility not checked yet')
  parts.push(`sha256 ${entry.sha256.slice(0, 12)}…`)
  if (isStuck(entry) && !entry.enabled) parts.push('cannot be enabled on this Buz')
  else parts.push(entry.enabled ? 'click to disable' : 'click to enable')
  return parts.join(' · ')
}

function chipClass(entry: PoolEntry): string {
  if (isStuck(entry)) {
    return 'border-red-500/40 bg-red-500/10 text-red-600 dark:text-red-400 hover:bg-red-500/20'
  }
  if (entry.enabled) {
    return 'border-green-500/30 bg-green-500/10 text-green-700 dark:text-green-400 hover:bg-green-500/20'
  }
  return 'border-border/70 bg-muted/30 text-muted-foreground hover:bg-muted hover:text-foreground'
}

/** Friendly refusal for a staged incompatible version — the gate would
 *  reject it anyway; show why before the click ever reaches the API. */
async function warnIncompatible(entry: PoolEntry) {
  const name = `${entry.namespace}/${entry.step_id}`
  const reason =
    entry.compat_error ?? 'This template is not compatible with this manager build.'
  await dialog.alert(
    `v${entry.version} of ${name} can't be enabled on this Buz.\n\n${reason}\n\n` +
      'Pick an older version, or ask an administrator to upgrade Buz.',
  )
}

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
  busy.value = entryKey(entry)
  try {
    await stepSourcesApi.togglePoolEntry(
      entry.namespace,
      entry.step_id,
      entry.version,
      !entry.enabled,
    )
    await load()
  } catch (e) {
    await dialog.alert(
      `Failed to ${entry.enabled ? 'disable' : 'enable'} v${entry.version}: ` +
        (e instanceof Error ? e.message : e),
    )
  } finally {
    busy.value = ''
  }
}

/** Chip click = the enable/disable switch. Incompatible staged versions
 *  never reach the API — they get the friendly explanation instead. (A
 *  version enabled before a Buz upgrade stays clickable so it can be
 *  disabled again.) */
async function onChipClick(entry: PoolEntry) {
  if (isStuck(entry) && !entry.enabled) {
    await warnIncompatible(entry)
    return
  }
  await toggleEntry(entry)
}

onMounted(load)
</script>

<template>
  <div class="space-y-6">
    <!-- ── Pool: one plugin per group, versions as clickable chips ── -->
    <section class="rounded-xl border border-border/50 bg-card overflow-hidden">
      <div class="px-5 py-3 border-b border-border/50">
        <p class="text-xs text-muted-foreground">
          Reference with <code class="font-mono text-[10px]">uses:</code>. Click a version to enable or disable it.
          Auto-enable sources initialize new plugins with their latest compatible version enabled.
        </p>
      </div>
      <div class="divide-y divide-border/50">
        <div
          v-for="group in pageItems"
          :key="`${group.namespace}/${group.stepId}`"
          class="px-5 py-4"
        >
          <div class="flex items-center justify-between gap-3">
            <div class="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5">
              <code class="font-mono text-xs font-semibold">{{ group.namespace }}/{{ group.stepId }}</code>
              <span
                v-if="group.updateAvailable"
                class="rounded-full bg-amber-500/10 px-2 py-0.5 text-[11px] text-amber-600 dark:text-amber-400"
              >update available</span>
              <span class="truncate text-[11px] text-muted-foreground">source: {{ group.sourceName }}</span>
            </div>
            <UiButton
              size="sm"
              variant="ghost"
              class="shrink-0"
              :disabled="refreshing === `${group.namespace}/${group.stepId}`"
              title="Re-fetch this plugin's versions from its source"
              :aria-label="`Refresh ${group.namespace}/${group.stepId}`"
              @click="refreshGroup(group)"
            ><RefreshCw
              class="h-3.5 w-3.5"
              :class="refreshing === `${group.namespace}/${group.stepId}` ? 'animate-spin' : ''"
            /></UiButton>
          </div>
          <div class="mt-2 flex flex-wrap gap-1.5">
            <button
              v-for="entry in group.versions"
              :key="entryKey(entry)"
              type="button"
              class="inline-flex items-center gap-1 rounded-full border px-2.5 py-1 font-mono text-xs transition-colors"
              :class="[chipClass(entry), busy === entryKey(entry) ? 'pointer-events-none opacity-50' : '']"
              :title="chipTitle(entry)"
              @click="onChipClick(entry)"
            >
              <AlertTriangle v-if="isStuck(entry)" class="h-3 w-3 shrink-0" />
              <Check v-else-if="entry.reviewed" class="h-3 w-3 shrink-0" />
              v{{ entry.version }}
            </button>
          </div>
        </div>
        <p v-if="poolGroups.length === 0" class="px-5 py-4 text-sm text-muted-foreground">
          Pool is empty — sync a source to stage templates for curation.
        </p>
      </div>
      <TablePagination
        v-model:page="page"
        v-model:page-size="pageSize"
        :total="total"
        :page-count="pageCount"
        :range-label="rangeLabel"
      />
    </section>
  </div>
</template>
