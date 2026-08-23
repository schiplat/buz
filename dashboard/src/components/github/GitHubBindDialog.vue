<script setup lang="ts">
import { ref, watch, computed } from 'vue'
import UiButton from '@/components/ui/UiButton.vue'
import { Loader2, RefreshCw, Trash2, GitPullRequestArrow, X } from '@lucide/vue'
import { githubSyncApi, type GitHubBind, type GitHubRepo, type GitHubPushResult } from '@/api/githubSync'
import { formatLocalDateTime } from '@/utils/time'

const props = defineProps<{
  show: boolean
  /** `secret` or `variable` — decides which bind API to call. */
  resourceKind: 'secret' | 'variable'
  resourceId: string
  resourceName: string
  /** Platform admin (variables) or secret manager (owner/maintainer/admin). */
  canManage: boolean
}>()

const emit = defineEmits<{ 'update:show': [value: boolean]; changed: [] }>()

const binds = ref<GitHubBind[]>([])
const repos = ref<GitHubRepo[]>([])
const loading = ref(false)
const pushing = ref(false)
const error = ref('')
const selectedRepoIds = ref<string[]>([])
const repoSearch = ref('')
const adding = ref(false)

const availableRepos = computed(() => {
  const boundRepoIds = new Set(binds.value.map((b) => b.repo_id))
  return repos.value.filter((r) => !boundRepoIds.has(r.id))
})

const filteredAvailableRepos = computed(() => {
  const q = repoSearch.value.trim().toLowerCase()
  let list = availableRepos.value
  if (q) {
    list = list.filter((r) => `${r.owner}/${r.repo} ${r.connection_name}`.toLowerCase().includes(q))
  }
  return list
})

function repoById(id: string): GitHubRepo | undefined {
  return repos.value.find((r) => r.id === id)
}

function repoLabel(id: string) {
  const r = repoById(id)
  return r ? `${r.owner}/${r.repo}` : id
}

function removeSelectedRepo(id: string) {
  selectedRepoIds.value = selectedRepoIds.value.filter((x) => x !== id)
}

const hasFailed = computed(() => binds.value.some((b) => b.last_status === 'failed'))

function close() {
  emit('update:show', false)
}

async function load() {
  if (!props.show) return
  loading.value = true
  error.value = ''
  try {
    const [bindList, repoList] = await Promise.all([
      props.resourceKind === 'secret'
        ? githubSyncApi.listSecretBinds(props.resourceId)
        : githubSyncApi.listVariableBinds(props.resourceId),
      githubSyncApi.listRepos(),
    ])
    binds.value = bindList
    repos.value = repoList
    selectedRepoIds.value = []
    repoSearch.value = ''
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to load GitHub bindings'
  } finally {
    loading.value = false
  }
}

async function handleAdd() {
  const ids = [...selectedRepoIds.value]
  if (ids.length === 0) return
  adding.value = true
  const failures: string[] = []
  const results = await Promise.allSettled(
    ids.map((repoId) =>
      props.resourceKind === 'secret'
        ? githubSyncApi.addSecretBind(props.resourceId, repoId)
        : githubSyncApi.addVariableBind(props.resourceId, repoId),
    ),
  )
  results.forEach((res, i) => {
    if (res.status === 'rejected') {
      const e: any = res.reason
      failures.push(
        `${repoLabel(ids[i])} — ${e?.response?.data?.message || e?.message || 'Failed to add binding'}`,
      )
    }
  })
  selectedRepoIds.value = []
  repoSearch.value = ''
  emit('changed')
  // load() clears the error, so set the failure summary after the reload.
  await load()
  if (failures.length > 0) {
    error.value = `Bind failed for: ${failures.join('; ')}`
  }
  adding.value = false
}

async function handleRemove(bind: GitHubBind) {
  if (!confirm(`Unbind from ${bind.owner}/${bind.repo}? The value is removed from GitHub Actions and the local binding is deleted.`)) return
  error.value = ''
  try {
    if (props.resourceKind === 'secret') {
      await githubSyncApi.deleteSecretBind(props.resourceId, bind.id)
    } else {
      await githubSyncApi.deleteVariableBind(props.resourceId, bind.id)
    }
    emit('changed')
    await load()
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Failed to unbind'
  }
}

async function handlePush(bind?: GitHubBind) {
  pushing.value = true
  error.value = ''
  try {
    const resp: { results: GitHubPushResult[] } =
      props.resourceKind === 'secret'
        ? await githubSyncApi.pushSecretBinds(props.resourceId, bind ? { bind_id: bind.id } : undefined)
        : await githubSyncApi.pushVariableBinds(props.resourceId, bind ? { bind_id: bind.id } : undefined)
    const failed = resp.results.filter((r) => r.status === 'failed')
    if (failed.length > 0) {
      error.value = `Push failed for: ${failed.map((f) => `${f.owner}/${f.repo ?? ''} — ${f.error || 'unknown'}`).join('; ')}`
    }
    emit('changed')
    await load()
  } catch (e: any) {
    error.value = e?.response?.data?.message || e?.message || 'Push failed'
  } finally {
    pushing.value = false
  }
}

watch(
  () => props.show,
  (v) => {
    if (v) void load()
  },
  { immediate: true },
)
</script>

<template>
  <Teleport to="body">
    <div v-if="show" class="fixed inset-0 z-50 flex items-center justify-center">
      <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="close" />
      <div class="relative z-10 w-full max-w-lg mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl flex flex-col max-h-[min(85vh,calc(100vh-4rem))]">
        <h3 class="shrink-0 text-[15px] font-semibold px-6 pt-6 pb-4 flex items-center gap-2">
          <GitPullRequestArrow class="h-4 w-4 text-muted-foreground" />
          GitHub Sync — <span class="font-mono">{{ resourceName }}</span>
        </h3>

        <div v-if="loading" class="flex items-center justify-center gap-2 py-12 text-muted-foreground text-sm">
          <Loader2 class="h-4 w-4 animate-spin" /> Loading…
        </div>

        <div v-else class="flex-1 min-h-0 overflow-y-auto px-6 space-y-4">
          <p v-if="!canManage" class="rounded-lg bg-muted/50 px-3 py-2 text-[11px] text-muted-foreground">
            Read-only — binding and push require platform admin (variables) or an owner/maintainer
            role in this secret's team.
          </p>

          <div v-if="binds.length === 0" class="rounded-xl border border-dashed border-border/60 px-4 py-6 text-center">
            <p class="text-xs text-muted-foreground">No GitHub repos bound yet</p>
            <p class="text-[11px] text-muted-foreground/60 mt-1">
              Values are only sent to GitHub when you push a binding.
            </p>
          </div>

          <div v-else class="space-y-2">
            <div v-for="b in binds" :key="b.id" class="rounded-xl border border-border/50 bg-muted/20 p-3">
              <div class="flex items-center gap-2 flex-wrap">
                <span class="font-mono text-[13px] font-semibold">{{ b.owner }}/{{ b.repo }}</span>
                <span
                  class="inline-flex items-center gap-1 rounded-md border px-1.5 py-0.5 text-[10px] font-medium"
                  :class="
                    b.last_status === 'ok'
                      ? 'bg-emerald-500/10 text-emerald-700 dark:text-emerald-400 border-emerald-500/20'
                      : b.last_status === 'failed'
                        ? 'bg-red-500/10 text-red-700 dark:text-red-400 border-red-500/20'
                        : 'bg-amber-500/10 text-amber-700 dark:text-amber-400 border-amber-500/20'
                  "
                >
                  {{
                    b.last_op === 'unbind' && b.last_status === 'failed'
                      ? 'unbind failed'
                      : b.last_status === 'pending'
                        ? 'not pushed'
                        : b.last_status
                  }}
                </span>
                <span class="text-[10px] text-muted-foreground ml-auto">{{ b.connection_name }}</span>
              </div>
              <p v-if="b.last_error" class="mt-1.5 text-[11px] text-red-600 dark:text-red-400 break-words">{{ b.last_error }}</p>
              <p v-else-if="b.last_pushed_at" class="mt-1.5 text-[11px] text-muted-foreground">
                Last pushed {{ formatLocalDateTime(b.last_pushed_at) }}
              </p>
              <div v-if="canManage" class="mt-2 flex items-center gap-2">
                <UiButton size="sm" variant="outline" :disabled="pushing" @click="handlePush(b)">
                  <RefreshCw class="h-3 w-3" /> Push
                </UiButton>
                <UiButton size="sm" variant="ghost" @click="handleRemove(b)" title="Unbind">
                  <Trash2 class="h-3 w-3 text-red-500" /> Unbind
                </UiButton>
              </div>
            </div>
          </div>

          <div v-if="canManage" class="rounded-xl border border-border/50 bg-muted/20 p-3">
            <p class="text-[11px] font-medium text-foreground mb-2">Bind repos</p>
            <input
              v-model="repoSearch"
              type="text"
              placeholder="Search repos…"
              class="w-full rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50 focus:ring-1 focus:ring-primary/10"
            />
            <div class="mt-2 max-h-36 overflow-y-auto rounded-lg border border-border/60 bg-background divide-y divide-border/20">
              <label
                v-for="r in filteredAvailableRepos"
                :key="r.id"
                class="flex cursor-pointer items-center gap-2 px-3 py-1.5 text-xs transition-colors hover:bg-muted/40"
              >
                <input v-model="selectedRepoIds" type="checkbox" :value="r.id" class="accent-primary shrink-0" />
                <span class="font-mono">{{ r.owner }}/{{ r.repo }}</span>
                <span class="ml-auto text-[10px] text-muted-foreground truncate">{{ r.connection_name }}</span>
              </label>
              <p v-if="filteredAvailableRepos.length === 0" class="px-3 py-2 text-[11px] text-muted-foreground">
                No unbound repos — register repos in Infrastructure → GitHub Sync first.
              </p>
            </div>
            <div v-if="selectedRepoIds.length > 0" class="mt-2 flex flex-wrap gap-1">
              <span
                v-for="id in selectedRepoIds"
                :key="id"
                class="inline-flex items-center gap-1 rounded-md border border-primary/30 bg-primary/5 px-1.5 py-0.5 font-mono text-[10px]"
              >
                {{ repoLabel(id) }}
                <button
                  type="button"
                  class="text-muted-foreground transition-colors hover:text-foreground"
                  :title="`Remove ${repoLabel(id)}`"
                  @click="removeSelectedRepo(id)"
                >
                  <X class="h-3 w-3" />
                </button>
              </span>
            </div>
            <div class="mt-2">
              <UiButton size="sm" :disabled="selectedRepoIds.length === 0 || adding" @click="handleAdd">
                {{ adding ? 'Binding…' : selectedRepoIds.length > 0 ? `Bind ${selectedRepoIds.length} repo${selectedRepoIds.length > 1 ? 's' : ''}` : 'Bind' }}
              </UiButton>
            </div>
          </div>
        </div>

        <div v-if="error" class="shrink-0 px-6 pb-2">
          <p class="rounded-lg bg-red-500/10 border border-red-500/20 px-3 py-2 text-[11px] text-red-700 dark:text-red-400 break-words">{{ error }}</p>
        </div>

        <div class="shrink-0 flex items-center gap-3 px-6 py-5 border-t border-border/30">
          <UiButton v-if="canManage && binds.length > 0" size="sm" :disabled="pushing" title="Push all bound repos" @click="handlePush()">
            <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': pushing }" />
            {{ pushing ? 'Pushing…' : 'Push All' }}
          </UiButton>
          <span v-if="hasFailed" class="text-[10px] text-red-500">
            Some pushes failed — check the connection / repo and retry.
          </span>
          <UiButton variant="ghost" size="sm" class="ml-auto" @click="close">Close</UiButton>
        </div>
      </div>
    </div>
  </Teleport>
</template>
