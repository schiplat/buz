<script setup lang="ts">
import { ref, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { KeyRound, Plus, Trash2, X, Copy, Check } from '@lucide/vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import { apiTokensApi, type ApiToken } from '@/api/apiTokens'
import { useAuthStore } from '@/stores/auth'
import { copyText } from '@/utils/copyText'

const auth = useAuthStore()
const router = useRouter()

const loading = ref(true)
const tokens = ref<ApiToken[]>([])
const showCreate = ref(false)
const newName = ref('')
const creating = ref(false)
const createdToken = ref('')
const createdTokenName = ref('')
const copied = ref(false)

async function load() {
  if (!auth.isPlatformAdmin) {
    router.replace('/dashboard')
    return
  }
  loading.value = true
  try {
    const res = await apiTokensApi.list()
    tokens.value = res.items ?? []
  } catch {
    tokens.value = []
  } finally {
    loading.value = false
  }
}

function openCreate() {
  newName.value = ''
  createdToken.value = ''
  copied.value = false
  showCreate.value = true
}

async function handleCreate() {
  if (!newName.value.trim()) return
  creating.value = true
  try {
    const res = await apiTokensApi.create(newName.value.trim())
    createdTokenName.value = res.name
    createdToken.value = res.token
  } finally {
    creating.value = false
  }
}

async function handleCopy() {
  const ok = await copyText(createdToken.value)
  if (ok) {
    copied.value = true
    setTimeout(() => (copied.value = false), 2000)
  }
}

async function handleRevoke(t: ApiToken) {
  if (!confirm(`Revoke API token "${t.name}"? This cannot be undone.`)) return
  await apiTokensApi.revoke(t.id)
  await load()
}

function closeCreate() {
  showCreate.value = false
  if (createdToken.value) void load()
}

onMounted(load)
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      title="API tokens"
      description="Long-lived credentials for scripts and tooling. Each token acts as the creating admin for API access; super admins can see all tokens."
    >
      <template #actions>
        <UiButton size="sm" @click="openCreate"><Plus class="h-4 w-4" /> New Token</UiButton>
      </template>
    </PageHeader>

    <div v-if="loading" class="surface-card p-8 text-center type-meta">
      Loading…
    </div>

    <div v-else class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
      <div v-if="tokens.length === 0" class="flex flex-col items-center justify-center py-16 text-muted-foreground">
        <KeyRound class="h-10 w-10 mb-3 text-muted-foreground/20" />
        <p class="text-sm">No API tokens</p>
        <p class="text-xs mt-1">Create one to authenticate scripts against the Manager API.</p>
      </div>
      <template v-else>
        <div class="overflow-x-auto">
          <table class="w-full text-[13px]">
            <thead>
              <tr class="border-b border-border/30 bg-muted/10">
                <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Name</th>
                <th v-if="auth.isSuperAdmin" class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Owner</th>
                <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Token</th>
                <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Created</th>
                <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Last used</th>
                <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Status</th>
                <th class="text-right px-5 py-2.5 w-16" />
              </tr>
            </thead>
            <tbody class="divide-y divide-border/20">
              <tr v-for="t in tokens" :key="t.id" class="hover:bg-muted/5">
                <td class="px-5 py-3 font-semibold">{{ t.name }}</td>
                <td v-if="auth.isSuperAdmin" class="px-5 py-3 text-xs text-muted-foreground">{{ t.username || '—' }}</td>
                <td class="px-5 py-3 font-mono text-xs text-muted-foreground">{{ t.prefix }}</td>
                <td class="px-5 py-3 text-xs">{{ t.created_at?.slice(0, 19) || '—' }}</td>
                <td class="px-5 py-3 text-xs text-muted-foreground">{{ t.last_used_at?.slice(0, 19) || 'never' }}</td>
                <td class="px-5 py-3 text-xs">
                  <span :class="t.revoked ? 'text-red-500' : 'text-emerald-500'">{{ t.revoked ? 'revoked' : 'active' }}</span>
                </td>
                <td class="px-5 py-3 text-right">
                  <UiButton v-if="!t.revoked" variant="ghost" size="sm" @click="handleRevoke(t)">
                    <Trash2 class="h-3.5 w-3.5 text-red-500" />
                  </UiButton>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
      </template>
    </div>

    <Teleport to="body">
      <div v-if="showCreate" class="fixed inset-0 z-50 flex items-start justify-center pt-[15vh]" @click.self="closeCreate">
        <div class="soft-panel w-full max-w-md p-6 space-y-5">
          <div class="flex items-center justify-between">
            <h2 class="text-base font-semibold tracking-tight">New API Token</h2>
            <button type="button" class="inline-flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground" @click="closeCreate">
              <X class="h-4 w-4" />
            </button>
          </div>

          <template v-if="!createdToken">
            <div>
              <label class="type-label block mb-1.5">Name <span class="text-red-500">*</span></label>
              <input
                v-model="newName"
                type="text"
                placeholder="e.g. ci-script"
                class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
                @keyup.enter="handleCreate"
              />
              <p class="text-xs text-muted-foreground mt-1.5">The token is shown only once after creation. Store it somewhere safe.</p>
            </div>
            <div class="flex justify-end gap-3">
              <UiButton variant="outline" @click="closeCreate">Cancel</UiButton>
              <UiButton :disabled="creating || !newName.trim()" @click="handleCreate">{{ creating ? 'Creating...' : 'Create Token' }}</UiButton>
            </div>
          </template>

          <template v-else>
            <div class="space-y-4">
              <div class="rounded-xl bg-muted px-4 py-3">
                <p class="type-eyebrow mb-1.5">Token ({{ createdTokenName }}) — copy it now, it will not be shown again</p>
                <p class="font-mono text-sm break-all text-foreground">{{ createdToken }}</p>
              </div>
              <div class="flex justify-end gap-3">
                <UiButton variant="outline" @click="handleCopy">
                  <Check v-if="copied" class="h-4 w-4 text-emerald-500" />
                  <Copy v-else class="h-4 w-4" />
                  {{ copied ? 'Copied' : 'Copy' }}
                </UiButton>
                <UiButton @click="closeCreate">Done</UiButton>
              </div>
            </div>
          </template>
        </div>
      </div>
    </Teleport>
  </div>
</template>
