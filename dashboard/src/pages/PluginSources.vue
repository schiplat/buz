<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import { RefreshCw, RotateCcw, Trash2 } from '@lucide/vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import { stepSourcesApi, type StepSource } from '@/api/stepSources'
import { adminSettingsApi, type PlatformSetting } from '@/api/adminSettings'
import { useAppDialog } from '@/composables/useAppDialog'
import { useRouter } from 'vue-router'

const dialog = useAppDialog()
const router = useRouter()
const loading = ref(true)
const sources = ref<StepSource[]>([])
const settings = ref<PlatformSetting[]>([])
const settingDrafts = ref<Record<string, string>>({})
interface SettingMeta {
  label: string
  /** One concise sentence: what it is / what it does. */
  help: string
  placeholder?: string
  /** Input width for non-bool rows. */
  narrow?: boolean
  /** Unit suffix rendered inside the input row. */
  unit?: string
}

const settingsMeta: Record<string, SettingMeta> = {
  step_review_required: {
    label: 'Step review gate',
    help: 'When on, saving a pipeline that uses a plugin not listed in the reviewed manifest is rejected. The manifest comes from the official buz-plugins review ledger — a template needs a human review (hash-locked) before it can be used. Requires the manifest file to exist; otherwise every save is rejected (the banner warns). Recommended on for production.',
  },
  step_index_local_dir: {
    label: 'Local plugin directory',
    help: 'Where local plugin YAMLs are read from. Git-clone buz-plugins here for offline use.',
  },
  step_reviewed_manifest: {
    label: 'Reviewed manifest',
    help: 'The official review ledger (reviews.json). Required by the review gate.',
  },
  step_sync_timeout: {
    label: 'Source sync timeout',
    help: 'Per-request timeout for source fetches. 0 disables — a hung source then blocks saves.',
    placeholder: '60',
    narrow: true,
    unit: 's',
  },
}

/** Display grouping: line-free sections carried by the small-caps caption. */
const settingGroups: Array<{ caption: string; keys: string[] }> = [
  { caption: 'Review gate', keys: ['step_review_required'] },
  { caption: 'Paths', keys: ['step_index_local_dir', 'step_reviewed_manifest'] },
  { caption: 'Sync', keys: ['step_sync_timeout'] },
]
const savingSetting = ref('')
const showAddSource = ref(false)
const newName = ref('')
const newUrl = ref('')
const newAutoEnable = ref(false)
const busy = ref('')
/** sourceId -> the last_synced_at value observed when Sync was clicked.
 *  While the row still shows that same value, the background sync is in
 *  flight ("syncing…"); a changed value (success OR failure — the backend
 *  stamps last_synced_at either way) resolves it. */
const syncingSince = ref<Record<string, string>>({})
/** sourceId -> click epoch ms; hard cap for the in-flight display. */
const syncingStartedAt = ref<Record<string, number>>({})
let pollTimer: ReturnType<typeof setInterval> | null = null

/** Refresh the list every 2.5s until every source reached a terminal sync
 *  state (last_synced_at or last_sync_error) — background syncs surface
 *  here instead of blocking the add-source flow. */
function startPolling() {
  stopPolling()
  let rounds = 0
  pollTimer = setInterval(async () => {
    rounds += 1
    await load()
    // Poll exactly as long as something is in flight — a source holding an
    // old timestamp is NOT pending (the previous condition stopped polling
    // while the syncing badge was still displayed).
    const pending = Object.keys(syncingSince.value).length > 0
    if (!pending || rounds >= 60) stopPolling()
  }, 2500)
}

function stopPolling() {
  if (pollTimer) {
    clearInterval(pollTimer)
    pollTimer = null
  }
}

async function load() {
  loading.value = true
  try {
    const [src, cfg] = await Promise.all([
      stepSourcesApi.listSources(),
      adminSettingsApi.list().catch(() => ({ items: [] as PlatformSetting[] })),
    ])
    sources.value = src.items ?? []
    // A sync is finished once last_synced_at differs from the value seen at
    // click time (the backend stamps it on success AND failure). A hard cap
    // (backend timeout + margin) resolves the display even when the stamped
    // value equals the baseline — the loaded row then shows the real state.
    const startedCap = syncTimeoutMs() + 5_000
    for (const s of sources.value) {
      const started = syncingStartedAt.value[s.id]
      const changed =
        syncingSince.value[s.id] !== undefined &&
        s.last_synced_at !== syncingSince.value[s.id]
      if (started !== undefined && (changed || Date.now() - started > startedCap)) {
        delete syncingSince.value[s.id]
        delete syncingStartedAt.value[s.id]
      }
    }
    settings.value = cfg.items ?? []
    settingDrafts.value = Object.fromEntries(settings.value.map((s) => [s.key, s.value]))
  } catch (e) {
    await dialog.alert(`Failed to load sources: ${e instanceof Error ? e.message : e}`)
  } finally {
    loading.value = false
  }
}

async function persistSetting(setting: PlatformSetting, value: string) {
  savingSetting.value = setting.key
  try {
    await adminSettingsApi.update(setting.key, value)
    setting.value = value
    setting.overridden = true
    settingDrafts.value[setting.key] = value
    justSaved.value = setting.key
    if (justSavedTimer) clearTimeout(justSavedTimer)
    justSavedTimer = setTimeout(() => (justSaved.value = ''), 1500)
  } catch (e) {
    await dialog.alert(`Failed to save ${setting.key}: ${e instanceof Error ? e.message : e}`)
  } finally {
    savingSetting.value = ''
  }
}

async function persistSettingDraft(setting: PlatformSetting) {
  await persistSetting(setting, settingDrafts.value[setting.key] ?? '')
}

async function resetSetting(setting: PlatformSetting) {
  savingSetting.value = setting.key
  try {
    await adminSettingsApi.reset(setting.key)
    setting.value = setting.default
    setting.overridden = false
    settingDrafts.value[setting.key] = setting.default
  } catch (e) {
    await dialog.alert(`Failed to reset ${setting.key}: ${e instanceof Error ? e.message : e}`)
  } finally {
    savingSetting.value = ''
  }
}

function settingByKey(key: string): PlatformSetting | undefined {
  return settings.value.find((s) => s.key === key)
}

/** Key of the setting that just saved — drives the ✓ / green border flash. */
const justSaved = ref('')
let justSavedTimer: ReturnType<typeof setTimeout> | null = null

function syncTimeoutMs(): number {
  const setting = settings.value.find((s) => s.key === 'step_sync_timeout')
  const secs = Number(setting?.value)
  return (Number.isFinite(secs) && secs > 0 ? secs : 60) * 1000
}

async function addSource() {
  if (!newName.value.trim() || !newUrl.value.trim()) return
  busy.value = 'add'
  try {
    await stepSourcesApi.createSource(
      newName.value.trim(),
      newUrl.value.trim(),
      newAutoEnable.value,
    )
    // Back to the list immediately: the sync runs server-side and the row
    // shows "syncing…" until last_synced_at / last_sync_error lands.
    showAddSource.value = false
    newName.value = ''
    newUrl.value = ''
    newAutoEnable.value = false
    await load()
    startPolling()
  } catch (e) {
    await dialog.alert(`Failed to add source: ${e instanceof Error ? e.message : e}`)
  } finally {
    busy.value = ''
  }
}

async function syncSource(source: StepSource) {
  busy.value = `sync-${source.id}`
  syncingSince.value[source.id] = source.last_synced_at ?? ''
  syncingStartedAt.value[source.id] = Date.now()
  try {
    await stepSourcesApi.syncSource(source.id)
    await load()
    startPolling()
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

function syncState(source: StepSource): string {
  const started = syncingStartedAt.value[source.id]
  const inFlight =
    source.id in syncingSince.value &&
    source.last_synced_at === syncingSince.value[source.id] &&
    (started === undefined || Date.now() - started <= syncTimeoutMs() + 5_000)
  if (inFlight) return 'syncing…'
  if (source.last_sync_error) return `sync error: ${source.last_sync_error}`
  if (source.last_synced_at) return `synced ${source.last_synced_at}`
  return 'syncing…'
}

onMounted(load)

onUnmounted(stopPolling)
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      title="Sources"
      description="Where plugin templates come from. Syncing stages pool entries; curation happens in the Pool tab. A source may also publish reviews.json beside its index — the review ledger (which content hashes were human-reviewed) — distinct from the index itself (what exists and where)."
    />

    <section class="rounded-xl border border-border/50 bg-card overflow-hidden">
      <div class="flex items-center justify-between px-5 py-4 border-b border-border/50">
        <h2 class="text-sm font-semibold">Configured sources</h2>
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

    <!-- ── Platform settings (line-free: hierarchy via spacing, captions,
         and the inputs' own borders) ── -->
    <section class="rounded-xl border border-border/50 bg-card px-6 py-5">
      <div class="flex items-baseline justify-between gap-4">
        <h2 class="text-sm font-semibold">Settings</h2>
        <p class="text-[11px] text-muted-foreground">
          DB overrides <code class="font-mono text-[10px]">manager.toml</code> ·
          <span class="font-medium text-foreground">Reset falls back to the file</span>
        </p>
      </div>

      <div v-for="group in settingGroups" :key="group.caption" class="mt-6 first:mt-5">
        <p class="text-[11px] font-medium uppercase tracking-widest text-muted-foreground/70">
          {{ group.caption }}
        </p>

        <template v-for="key in group.keys" :key="key">
          <div v-if="settingByKey(key)" class="mt-3">
            <div class="flex items-center gap-2">
              <h3 class="text-sm font-medium">
                {{ settingsMeta[key]?.label ?? key }}
              </h3>
              <span
                v-if="settingByKey(key)!.overridden"
                class="rounded-full bg-primary/10 px-1.5 py-px text-[10px] text-primary"
              >overridden</span>
            </div>
            <p
              class="mt-0.5 max-w-2xl text-xs leading-relaxed text-muted-foreground"
              :class="settingsMeta[key]?.help.length > 90 ? '' : 'truncate'"
              :title="settingsMeta[key]?.help"
            >
              {{ settingsMeta[key]?.help }}
            </p>

            <div v-if="settingByKey(key)!.kind === 'bool'" class="mt-2.5 flex items-center gap-3">
              <button
                type="button"
                role="switch"
                :aria-checked="settingByKey(key)!.value === 'true'"
                :disabled="savingSetting === key"
                :class="[
                  'relative inline-flex h-5.5 w-10 shrink-0 cursor-pointer items-center rounded-full transition-colors',
                  settingByKey(key)!.value === 'true' ? 'bg-primary' : 'bg-muted-foreground/25',
                  savingSetting === key ? 'opacity-60' : '',
                ]"
                :data-testid="`switch-${key}`"
                @click="persistSetting(settingByKey(key)!, settingByKey(key)!.value === 'true' ? 'false' : 'true')"
              >
                <span
                  :class="[
                    'inline-block h-4 w-4 rounded-full bg-background shadow transition-transform',
                    settingByKey(key)!.value === 'true' ? 'translate-x-5' : 'translate-x-0.5',
                  ]"
                />
              </button>
              <span class="text-xs" :class="settingByKey(key)!.value === 'true' ? 'font-medium text-foreground' : 'text-muted-foreground'">
                {{ settingByKey(key)!.value === 'true' ? 'On' : 'Off' }}
              </span>
              <button
                v-if="settingByKey(key)!.overridden"
                type="button"
                class="text-[11px] text-muted-foreground underline-offset-2 hover:underline"
                @click="resetSetting(settingByKey(key)!)"
              >reset</button>
            </div>

            <div v-else class="mt-2.5">
              <div class="flex items-center gap-2">
                <input
                  v-model="settingDrafts[key]"
                  type="text"
                  :placeholder="settingsMeta[key]?.placeholder ?? settingByKey(key)!.default"
                  :class="[
                    'rounded-lg border border-border bg-input px-2.5 py-1.5 font-mono text-xs outline-hidden transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15',
                    settingsMeta[key]?.narrow ? 'w-20' : 'w-full',
                    justSaved === key ? 'border-green-500/50' : '',
                  ]"
                  :data-testid="`input-${key}`"
                />
                <span v-if="settingsMeta[key]?.unit" class="text-xs text-muted-foreground">
                  {{ settingsMeta[key]?.unit }}
                </span>
                <span v-if="justSaved === key" class="text-xs text-green-600 dark:text-green-400">✓</span>
                <button
                  v-if="settingDrafts[key] !== settingByKey(key)!.value"
                  type="button"
                  class="shrink-0 rounded-lg bg-primary px-3 py-1.5 text-xs font-medium text-primary-foreground transition-colors hover:bg-primary/90"
                  :disabled="savingSetting === key"
                  @click="persistSettingDraft(settingByKey(key)!)"
                >Save</button>
                <button
                  v-if="settingByKey(key)!.overridden"
                  type="button"
                  class="shrink-0 rounded-lg p-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                  title="Reset to the manager.toml default"
                  :data-testid="`reset-${key}`"
                  :disabled="savingSetting === key"
                  @click="resetSetting(settingByKey(key)!)"
                ><RotateCcw class="h-3.5 w-3.5" /></button>
              </div>
              <p
                v-if="!settingsMeta[key]?.narrow && (settingByKey(key)!.resolved || settingByKey(key)!.default)"
                class="mt-1.5 truncate font-mono text-[11px] text-muted-foreground/70"
                :title="settingByKey(key)!.overridden ? settingByKey(key)!.resolved : settingByKey(key)!.default"
              >{{ settingByKey(key)!.overridden ? settingByKey(key)!.resolved : settingByKey(key)!.default }}</p>
              <p
                v-if="settingsMeta[key]?.narrow"
                class="mt-1.5 text-[11px] text-muted-foreground/70"
              >5–300 seconds</p>
            </div>

            <p
              v-if="key === 'step_sync_timeout' && settingByKey(key)!.value === '0'"
              class="mt-2 text-[11px] font-medium text-red-600 dark:text-red-400"
              data-testid="timeout-disabled-warning"
            >
              ⚠️ Timeout disabled — a hung source will block pipeline saves indefinitely.
            </p>
          </div>
        </template>
      </div>
    </section>
  </div>
</template>
