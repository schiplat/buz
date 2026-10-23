<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { RotateCcw } from '@lucide/vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import { adminSettingsApi, type PlatformSetting } from '@/api/adminSettings'
import { useAuthStore } from '@/stores/auth'
import { useRouter } from 'vue-router'
import { useAppDialog } from '@/composables/useAppDialog'

const auth = useAuthStore()
const router = useRouter()
const dialog = useAppDialog()

const loading = ref(true)
const settings = ref<PlatformSetting[]>([])
const drafts = ref<Record<string, string>>({})
const savingKey = ref('')

/** Labels + help text per key — kept here (display concern) so the backend
 *  registry stays purely about validation and precedence. */
const meta: Record<string, { label: string; help: string }> = {
  marketplace_url: {
    label: 'Marketplace URL',
    help: 'Marketplace browsing entry. Leave empty to use the built-in default; point at an internal mirror for China-mainland or air-gapped deployments.',
  },
  step_review_required: {
    label: 'Step review gate (require_reviewed)',
    help: 'When enabled, templates absent from the official reviewed manifest are rejected at pipeline save time. Synced to the status banner immediately.',
  },
  step_reviewed_manifest: {
    label: 'Reviewed manifest',
    help: 'Path to reviews.json (the official review ledger). Empty = review gate disabled. The gate toggle above only rejects unreviewed templates when a manifest is configured.',
  },
  step_index_local_dir: {
    label: 'Local step directory',
    help: 'Directory scanned for template YAMLs (offline / private deployments). Changing it takes effect on the next scan.',
  },
}

async function load() {
  if (!auth.isPlatformAdmin) {
    router.replace('/dashboard')
    return
  }
  loading.value = true
  try {
    const res = await adminSettingsApi.list()
    settings.value = res.items ?? []
    drafts.value = Object.fromEntries(settings.value.map((s) => [s.key, s.value]))
  } catch (e) {
    await dialog.alert(`Failed to load settings: ${e instanceof Error ? e.message : e}`)
  } finally {
    loading.value = false
  }
}

async function persist(setting: PlatformSetting, value: string) {
  savingKey.value = setting.key
  try {
    await adminSettingsApi.update(setting.key, value)
    setting.value = value
    setting.overridden = true
    drafts.value[setting.key] = value
  } catch (e) {
    await dialog.alert(`Failed to save ${setting.key}: ${e instanceof Error ? e.message : e}`)
  } finally {
    savingKey.value = ''
  }
}

async function persistDraft(setting: PlatformSetting) {
  await persist(setting, drafts.value[setting.key] ?? '')
}

async function reset(setting: PlatformSetting) {
  savingKey.value = setting.key
  try {
    await adminSettingsApi.reset(setting.key)
    setting.value = setting.default
    setting.overridden = false
    drafts.value[setting.key] = setting.default
  } catch (e) {
    await dialog.alert(`Failed to reset ${setting.key}: ${e instanceof Error ? e.message : e}`)
  } finally {
    savingKey.value = ''
  }
}

onMounted(load)
</script>

<template>
  <div>
    <PageHeader title="Platform settings" description="Runtime overrides — DB values take precedence over manager.toml defaults." />

    <div v-if="loading" class="text-sm text-muted-foreground">Loading…</div>
    <div v-else class="max-w-3xl space-y-4">
      <div
        v-for="setting in settings"
        :key="setting.key"
        class="rounded-xl border border-border/50 bg-card p-5"
      >
        <div class="flex items-center justify-between gap-4">
          <div class="min-w-0">
            <div class="flex items-center gap-2">
              <h2 class="text-sm font-semibold">{{ meta[setting.key]?.label ?? setting.key }}</h2>
              <span
                v-if="setting.overridden"
                class="rounded-full bg-primary/10 px-2 py-0.5 text-[11px] text-primary"
              >overridden</span>
            </div>
            <p class="mt-1 text-xs leading-relaxed text-muted-foreground">{{ meta[setting.key]?.help }}</p>
          </div>

          <label
            v-if="setting.kind === 'bool'"
            class="flex cursor-pointer items-center gap-2 text-xs"
          >
            <span class="font-mono text-[11px] text-muted-foreground">{{ setting.value }}</span>
            <input
                  type="checkbox"
                  class="h-4 w-4"
                  :checked="setting.value === 'true'"
                  :disabled="savingKey === setting.key"
                  @change="persist(setting, ($event.target as HTMLInputElement).checked ? 'true' : 'false')"
            />
          </label>
        </div>

        <div v-if="setting.kind !== 'bool'" class="mt-3 flex items-center gap-2">
          <input
            v-model="drafts[setting.key]"
            :type="setting.kind === 'url' ? 'url' : 'text'"
            class="w-full rounded-xl border border-border bg-input px-3 py-2 font-mono text-xs outline-hidden transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
          />
          <UiButton
            size="sm"
            :disabled="savingKey === setting.key || drafts[setting.key] === setting.value"
            @click="persistDraft(setting)"
          >Save</UiButton>
          <UiButton
            size="sm"
            variant="ghost"
            :disabled="savingKey === setting.key || !setting.overridden"
            title="Reset to the manager.toml default"
            @click="reset(setting)"
          ><RotateCcw class="h-3.5 w-3.5" /> Reset</UiButton>
        </div>
      </div>

      <p class="text-xs text-muted-foreground">
        Defaults come from <code class="font-mono">manager.toml</code>; overrides persist in the
        database and survive restarts. Reset removes the override.
      </p>
    </div>
  </div>
</template>
