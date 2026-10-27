<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import {
  ArrowUpCircle,
  Bell,
  Check,
  Copy,
  GitBranch,
  Hammer,
  Package,
  Puzzle,
  RefreshCw,
  Search,
} from '@lucide/vue'
import PluginNotice from '@/components/PluginNotice.vue'
import UiButton from '@/components/ui/UiButton.vue'
import TablePagination from '@/components/ui/TablePagination.vue'
import { pluginSourcesApi, type PoolEntry } from '@/api/pluginSources'
import { useAppDialog } from '@/composables/useAppDialog'
import { useClientPagination } from '@/composables/useClientPagination'
import { formatLocalDateTime } from '@/utils/time'
import { buildPoolNotices, isDownloadFailure, isStuck } from '@/utils/poolNotices'

const dialog = useAppDialog()
const loading = ref(true)
const pool = ref<PoolEntry[]>([])
const busy = ref('')
const refreshing = ref('')
const search = ref('')
const category = ref('all')
/** uses: reference currently flashed as "copied" (Check icon feedback). */
const copiedUses = ref('')
let copiedTimer: ReturnType<typeof setTimeout> | null = null

/** Copy the bare uses reference (what you paste after `uses:` in a pipeline). */
async function copyUses(group: PoolGroup) {
  const reference = usesRef(group)
  try {
    await navigator.clipboard.writeText(reference)
  } catch {
    // Clipboard API unavailable (insecure context) — legacy fallback.
    const helper = document.createElement('textarea')
    helper.value = reference
    document.body.appendChild(helper)
    helper.select()
    document.execCommand('copy')
    helper.remove()
  }
  copiedUses.value = reference
  if (copiedTimer) clearTimeout(copiedTimer)
  copiedTimer = setTimeout(() => (copiedUses.value = ''), 1500)
}

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
  /** Descriptive meta of the newest version that carries one. */
  meta: PoolEntry['meta']
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
        meta: null,
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
    group.meta = group.versions.find((v) => v.meta)?.meta ?? null
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
    // "Update available" means a newer version than the one in use — with
    // nothing enabled there is no version to update *from*; that state is
    // plain "staged" and Enable is the action.
    group.updateAvailable =
      !!maxStaged && !!maxEnabled && compareTriple(maxStaged, maxEnabled) > 0
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

const categories = computed(() =>
  [...new Set(poolGroups.value.map((g) => g.meta?.category).filter((c): c is string => !!c))].sort(),
)

const searchLower = computed(() => search.value.trim().toLowerCase())

const filteredGroups = computed(() =>
  poolGroups.value.filter((group) => {
    if (category.value !== 'all' && (group.meta?.category ?? '') !== category.value) return false
    const q = searchLower.value
    if (!q) return true
    return (
      `${group.namespace}/${group.stepId}`.toLowerCase().includes(q) ||
      (group.meta?.name ?? '').toLowerCase().includes(q) ||
      (groupDescription(group) ?? '').toLowerCase().includes(q)
    )
  }),
)

const versionCount = computed(() =>
  filteredGroups.value.reduce((sum, group) => sum + group.versions.length, 0),
)

const { page, pageSize, pageCount, total, pageItems, rangeLabel } = useClientPagination(
  filteredGroups,
  { resetKey: groupSetKey },
)

// ── Card content helpers ─────────────────────────────────────────────────

const preferZh = () => navigator.language?.toLowerCase().startsWith('zh') ?? false

const isOfficial = (group: PoolGroup) => group.meta?.publisher === 'official'
const usesRef = (group: PoolGroup) => `${group.namespace}/${group.stepId}`
const groupTitle = (group: PoolGroup) => group.meta?.name?.trim() || usesRef(group)

function groupDescription(group: PoolGroup): string | null {
  const meta = group.meta
  if (!meta) return null
  const zh = meta.description_zh?.trim() || null
  const en = meta.description?.trim() || null
  return (preferZh() ? zh || en : en || zh) ?? null
}

const groupRequires = (group: PoolGroup) =>
  group.versions.find((v) => v.requires_buz)?.requires_buz ?? null

/** "enabled by alice · 10/27 12:31" for the version currently in use —
 *  the pool's answer to "who turned this on", without opening the audit log. */
function groupCuratorLine(group: PoolGroup): string | null {
  const enabled = group.versions.find((v) => v.enabled)
  if (!enabled?.updated_by) return null
  const when = enabled.updated_at ? formatLocalDateTime(enabled.updated_at, '') : ''
  return when ? `${enabled.updated_by} · ${when}` : enabled.updated_by
}

/** Compatibility tone for the requires-buz chip: green when some version of
 *  the plugin runs on this Manager build, red when none does, neutral while
 *  compatibility hasn't been checked. */
/** Text-only tone for the requires-buz note: green when some version runs on
 *  this build, red when none does, muted while unchecked. */
function groupCompatTone(group: PoolGroup): string {
  const states = group.versions.map((v) => v.buz_compatible)
  if (states.some((s) => s === true)) return 'text-emerald-600 dark:text-emerald-400'
  if (states.length > 0 && states.every((s) => s === false)) {
    return 'text-red-600 dark:text-red-400'
  }
  return 'text-muted-foreground'
}

function groupCompatTip(group: PoolGroup): string {
  const states = group.versions.map((v) => v.buz_compatible)
  if (states.some((s) => s === true)) {
    return 'At least one version is compatible with this Manager build'
  }
  if (states.length > 0 && states.every((s) => s === false)) {
    return 'No version is compatible with this Manager build'
  }
  return 'Compatibility not checked yet'
}

/** Category-tinted icon tile: a visual anchor for scanning the list. */
const categoryIcon = (group: PoolGroup) => {
  switch (group.meta?.category) {
    case 'build':
      return Hammer
    case 'source':
      return GitBranch
    case 'artifact':
      return Package
    case 'notify':
      return Bell
    default:
      return Puzzle
  }
}
const categoryIconClass = (group: PoolGroup) => {
  switch (group.meta?.category) {
    case 'build':
      return 'bg-blue-500/10 text-blue-600 dark:text-blue-400'
    case 'source':
      return 'bg-green-500/10 text-green-700 dark:text-green-400'
    case 'artifact':
      return 'bg-amber-500/10 text-amber-600 dark:text-amber-400'
    case 'notify':
      return 'bg-purple-500/10 text-purple-600 dark:text-purple-400'
    default:
      return 'bg-muted text-muted-foreground'
  }
}

const entryKey = (entry: PoolEntry) => `${entry.namespace}/${entry.step_id}/${entry.version}`

// ── Version dropdown + per-action buttons ─────────────────────────────────

/** Per-plugin selected version in the dropdown. Defaults to the enabled
 *  version (the active one), else the newest. */
const selectedVersions = ref<Record<string, string>>({})
watch(
  poolGroups,
  (groups) => {
    for (const group of groups) {
      const key = usesRef(group)
      const current = selectedVersions.value[key]
      if (!current || !group.versions.some((v) => v.version === current)) {
        const active = group.versions.find((v) => v.enabled) ?? group.versions[0]
        if (active) selectedVersions.value[key] = active.version
      }
    }
  },
  { immediate: true },
)

const selectedEntry = (group: PoolGroup): PoolEntry | null =>
  group.versions.find((v) => v.version === selectedVersions.value[usesRef(group)]) ?? null

const blockedRequirement = (entry: PoolEntry): string | null =>
  entry.compat_error?.match(/requires Buz (>=\s?\d+\.\d+\.\d+)/)?.[1] ?? null

/** Dropdown label: version plus the one bit that matters when scanning the
 *  list (in use or not). Review/compat detail lives on the state line under
 *  the select, where it cannot be truncated by a narrow rail. */
function versionOptionLabel(entry: PoolEntry): string {
  return `v${entry.version} — ${entry.enabled ? 'enabled' : 'staged'}`
}



/** Newest staged, compatible version that is strictly newer than everything
 *  enabled — the one-click upgrade target. Null when nothing is enabled:
 *  without a version in use there is nothing to upgrade from, and Enable
 *  already covers that case. */
function upgradeTarget(group: PoolGroup): PoolEntry | null {
  if (!group.updateAvailable) return null
  const candidate = group.versions.find((v) => !v.enabled && v.buz_compatible === true)
  if (!candidate) return null
  const candidateTriple = versionTriple(candidate.version)
  const enabledMax = group.versions
    .filter((v) => v.enabled)
    .map((v) => versionTriple(v.version))
    .filter((v): v is [number, number, number] => v !== null)
    .sort(compareTriple)
    .at(-1)
  if (!candidateTriple || !enabledMax) return null
  return compareTriple(candidateTriple, enabledMax) > 0 ? candidate : null
}

/** Problems for one plugin, as structured messages the card renders. */
function groupNotices(group: PoolGroup) {
  return buildPoolNotices(group.versions)
}

async function load() {
  loading.value = true
  try {
    const plt = await pluginSourcesApi.listPool()
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
    await pluginSourcesApi.refreshPlugin(group.namespace, group.stepId)
    await load()
  } catch (e) {
    await dialog.alert(`Refresh failed: ${e instanceof Error ? e.message : e}`)
  } finally {
    refreshing.value = ''
  }
}

/** Enable/disable the version currently picked in the dropdown. An
 *  incompatible staged version can't be enabled — the button is disabled and
 *  the card's alert strip already explains why; this guard just keeps the
 *  API unreachable from this path. */
async function toggleSelected(group: PoolGroup) {
  const entry = selectedEntry(group)
  if (!entry || (isStuck(entry) && !entry.enabled)) return
  busy.value = entryKey(entry)
  try {
    await pluginSourcesApi.togglePoolEntry(
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

/** One-click upgrade: enable the newest compatible staged version and retire
 *  every currently enabled one (both steps are plain curator toggles). */
async function upgrade(group: PoolGroup) {
  const target = upgradeTarget(group)
  if (!target) return
  busy.value = usesRef(group)
  try {
    await pluginSourcesApi.togglePoolEntry(
      target.namespace,
      target.step_id,
      target.version,
      true,
    )
    for (const old of group.versions.filter((v) => v.enabled)) {
      await pluginSourcesApi.togglePoolEntry(old.namespace, old.step_id, old.version, false)
    }
    await load()
  } catch (e) {
    await dialog.alert(`Upgrade failed: ${e instanceof Error ? e.message : e}`)
  } finally {
    busy.value = ''
  }
}

onMounted(load)
</script>

<template>
  <div class="space-y-6">
    <section class="rounded-xl border border-border/50 bg-card overflow-hidden">
      <!-- ── Toolbar: search + category filter + counts ── -->
      <div class="flex items-center gap-2.5 flex-wrap px-5 py-3 border-b border-border/50">
        <div class="relative flex-1 min-w-52 max-w-sm">
          <Search class="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground/60" aria-hidden="true" />
          <input
            v-model="search"
            type="search"
            placeholder="Search plugins…"
            class="w-full h-8 rounded-lg border border-border/60 bg-background pl-8 pr-3 text-xs outline-hidden focus:border-primary/50"
          />
        </div>
        <select
          v-model="category"
          class="h-8 rounded-lg border border-border/60 bg-background px-2 text-xs outline-hidden focus:border-primary/50"
          aria-label="Filter by category"
        >
          <option value="all">All categories</option>
          <option v-for="c in categories" :key="c" :value="c">{{ c }}</option>
        </select>
        <span class="ml-auto text-[11px] text-muted-foreground">
          {{ filteredGroups.length }} plugins · {{ versionCount }} versions
        </span>
      </div>

      <!-- ── Plugin cards: info left, actions in a right-hand rail ── -->
      <div class="divide-y divide-border/50">
        <div
          v-for="group in pageItems"
          :key="`${group.namespace}/${group.stepId}`"
          class="px-5 py-4 flex flex-col sm:flex-row gap-3.5 items-start"
        >
          <!-- Category icon tile -->
          <div
            class="shrink-0 h-10 w-10 rounded-[10px] flex items-center justify-center"
            :class="categoryIconClass(group)"
            aria-hidden="true"
          >
            <component :is="categoryIcon(group)" class="h-[19px] w-[19px]" />
          </div>

          <!-- Main: name / identity / description / alert -->
          <div class="flex-1 min-w-0">
            <div class="flex items-center gap-2 flex-wrap">
              <h3
                class="text-sm font-medium m-0"
                :class="group.meta?.name ? '' : 'font-mono text-[13px]'"
              >{{ groupTitle(group) }}</h3>
              <span v-if="group.meta?.category" class="text-[11px] text-muted-foreground/70">
                {{ group.meta.category }}
              </span>
              <span class="flex-1"></span>
              <UiButton
                size="icon"
                variant="ghost"
                :disabled="refreshing === `${group.namespace}/${group.stepId}`"
                :aria-label="`Refresh ${group.namespace}/${group.stepId}`"
                title="Re-fetch this plugin's versions from its source"
                @click="refreshGroup(group)"
              ><RefreshCw
                class="h-3.5 w-3.5"
                :class="refreshing === `${group.namespace}/${group.stepId}` ? 'animate-spin' : ''"
              /></UiButton>
            </div>
            <div class="mt-1 flex items-center gap-2 flex-wrap text-[11px] text-muted-foreground">
              <code class="font-mono bg-muted px-1.5 py-0.5 rounded-md text-foreground/70">uses: {{ usesRef(group) }}</code>
              <button
                type="button"
                class="inline-flex items-center justify-center h-5 w-5 rounded-md text-muted-foreground/70 hover:text-foreground hover:bg-muted transition-colors"
                :aria-label="`Copy ${usesRef(group)}`"
                title="Copy uses reference"
                @click="copyUses(group)"
              >
                <Check v-if="copiedUses === usesRef(group)" class="h-3 w-3 text-green-600" aria-hidden="true" />
                <Copy v-else class="h-3 w-3" aria-hidden="true" />
              </button>
              <span v-if="isOfficial(group)" class="font-medium text-primary">Official</span>
              <template v-else-if="group.meta?.author">
                <span>by {{ group.meta.author }}</span>
              </template>
              <span v-if="group.meta?.engine">{{ group.meta.engine }}</span>
              <span
                v-if="groupRequires(group)"
                class="font-medium"
                :class="groupCompatTone(group)"
                :title="groupCompatTip(group)"
              >requires Buz {{ groupRequires(group) }}</span>
              <span>via {{ group.sourceName }}</span>
            </div>

            <p
              v-if="groupDescription(group)"
              class="mt-1.5 mb-0 text-[13px] leading-relaxed text-muted-foreground line-clamp-2"
              :title="groupDescription(group) ?? undefined"
            >{{ groupDescription(group) }}</p>

            <!-- Structured problems only: nothing renders when everything is usable. -->
            <PluginNotice :notices="groupNotices(group)" />
          </div>

          <!-- ── Action rail: version, then one primary action ──
               Review/compat detail rides on the tooltips; the card shows a
               single amber line only when something is unusable. -->
          <div class="shrink-0 w-full sm:w-44 flex flex-col gap-1.5">
            <select
              v-model="selectedVersions[usesRef(group)]"
              class="w-full h-8 rounded-lg border border-border/60 bg-background px-2 text-xs font-mono outline-hidden focus:border-primary/50"
              :aria-label="`Version of ${usesRef(group)}`"
              :title="
                groupCuratorLine(group)
                  ? `Enable state last changed by ${groupCuratorLine(group)}`
                  : undefined
              "
            >
              <option v-for="entry in group.versions" :key="entry.version" :value="entry.version">
                {{ versionOptionLabel(entry) }}
              </option>
            </select>
            <UiButton
              size="sm"
              :variant="selectedEntry(group)?.enabled ? 'destructive-soft' : 'success'"
              class="w-full"
              :disabled="
                !selectedEntry(group) ||
                busy === entryKey(selectedEntry(group)!) ||
                (isStuck(selectedEntry(group)!) && !selectedEntry(group)!.enabled) ||
                (selectedEntry(group) ? isDownloadFailure(selectedEntry(group)!) : false)
              "
              :title="
                selectedEntry(group) && isDownloadFailure(selectedEntry(group)!)
                  ? 'The source does not serve this file — fix the source, then refresh'
                  : selectedEntry(group) && isStuck(selectedEntry(group)!) && !selectedEntry(group)!.enabled
                    ? 'This version needs a newer Buz — see the note on the left'
                    : selectedEntry(group)?.reviewed === false
                      ? 'Not in the reviewed manifest'
                      : undefined
              "
              @click="toggleSelected(group)"
            >{{ selectedEntry(group)?.enabled ? 'Disable' : 'Enable' }}</UiButton>
            <button
              v-if="upgradeTarget(group)"
              type="button"
              class="inline-flex h-6 items-center justify-center gap-1 rounded-lg text-[11px] font-medium text-muted-foreground transition-colors hover:text-foreground disabled:opacity-50"
              :disabled="busy === usesRef(group)"
              :title="`Enable v${upgradeTarget(group)!.version} and disable the currently enabled version`"
              @click="upgrade(group)"
            ><ArrowUpCircle class="h-3 w-3" /> Upgrade to v{{ upgradeTarget(group)!.version }}</button>
          </div>
        </div>
        <p v-if="filteredGroups.length === 0 && poolGroups.length > 0" class="px-5 py-4 text-sm text-muted-foreground">
          No plugins match your search or category filter.
        </p>
        <p v-else-if="poolGroups.length === 0" class="px-5 py-4 text-sm text-muted-foreground">
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
