<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import TablePagination from '@/components/ui/TablePagination.vue'
import { Plus, Pencil, Trash2, Shield, GitBranch } from '@lucide/vue'
import { secretsApi, normalizeSecretKind, validateSecretName, type Secret, type SecretKind } from '@/api/secrets'
import { teamsApi, type Team } from '@/api/teams'
import GitHubBindChips from '@/components/github/GitHubBindChips.vue'
import GitHubBindDialog from '@/components/github/GitHubBindDialog.vue'
import { useAuthStore } from '@/stores/auth'
import { useClientSort } from '@/composables/useClientSort'
import { useClientPagination } from '@/composables/useClientPagination'
import { formatLocalDateTime } from '@/utils/time'
import { useAppDialog } from '@/composables/useAppDialog'

const auth = useAuthStore()
const dialog = useAppDialog()
const secrets = ref<Secret[]>([])
const teams = ref<Team[]>([])
const loading = ref(true)
const searchQuery = ref('')
const filterTeam = ref('')
/** '' | 'needs_push' — pending / stale / failed binds. */
const filterGithub = ref('')

const filteredSecrets = computed(() => {
  let source = secrets.value.filter((s) => auth.matchesActiveTeam(s.team_id))
  if (filterTeam.value) source = source.filter((s) => s.team_id === filterTeam.value)
  if (filterGithub.value === 'needs_push') {
    source = source.filter((s) => needsGithubPush(s))
  }
  const q = searchQuery.value.trim().toLowerCase()
  if (!q) return source
  return source.filter((s) => {
    const teamName = teams.value.find(t => t.id === s.team_id)?.name || s.team_id || ''
    return [s.name, s.value, s.owner_group, s.kind, teamName].filter(Boolean).join(' ').toLowerCase().includes(q)
  })
})

const { sorted, toggleSort, sortIndicator } = useClientSort(filteredSecrets, {
  initialKey: 'name',
  initialDir: 'asc',
  getValue: (row, key) => {
    switch (key) {
      case 'name': return row.name
      case 'value': return row.value
      case 'team': {
        const t = teams.value.find(x => x.id === row.team_id)
        return t ? t.name : (row.team_id || '')
      }
      case 'team_shared': return row.team_shared ? 1 : 0
      case 'kind': return row.kind || 'generic'
      case 'github_status': return githubStatusValue(row)
      case 'updated_at': return row.updated_at || ''
      default: return ''
    }
  },
})

const { page, pageSize, pageCount, total, pageItems, rangeLabel } = useClientPagination(sorted)

// ── GitHub Sync bindings ──
const showGithubDialog = ref(false)
const githubTarget = ref<Secret | null>(null)

/** Sorting summary: none < failed < stale < pending < ok. */
function githubStatusValue(s: Secret): string {
  if (!s.github_binds?.length) return ''
  if (s.github_binds.some((b) => b.last_status === 'failed')) return 'failed'
  if (s.github_binds.some((b) => b.last_status === 'stale')) return 'stale'
  if (s.github_binds.some((b) => b.last_status === 'pending')) return 'pending'
  return 'ok'
}

function needsGithubPush(s: Secret): boolean {
  return !!s.github_binds?.some(
    (b) => b.last_status === 'pending' || b.last_status === 'stale' || b.last_status === 'failed',
  )
}

/** Secret binds need SecretManage: platform admin or owner/maintainer in the team. */
function canManageSecret(s: Secret): boolean {
  // GitHub binds push the secret to a repo with a connection PAT, so the
  // bind/unbind/push actions are platform-admin-only.
  return auth.isPlatformAdmin
}

function openGithub(s: Secret) {
  githubTarget.value = s
  showGithubDialog.value = true
}

function onGithubChanged() {
  void loadSecrets()
}

// ── Create / Edit dialog ──
const showDialog = ref(false)
const editing = ref<Secret | null>(null)
const formName = ref('')
const formValue = ref('')
const formTeamId = ref('')
const formTeamShared = ref(false)
const formKind = ref<SecretKind>('generic')
const saving = ref(false)
const showValue = ref(false)

const SECRET_REFERENCE = '${secrets.NAME}'
const SECRETS_DESCRIPTION = `Encrypted credentials. Reference ${SECRET_REFERENCE} in plugin with, step env, or config.render templates. Team share is optional; values are masked in logs.`

const DOCKER_LOGIN_EXAMPLE = `- name: Registry Login
  uses: buz/docker-login@1.1.0
  with:
    registry: crpi-xxxx.cn-hangzhou.personal.cr.aliyuncs.com
    username: \${secrets.REGISTRY_USERNAME}
    password: \${secrets.REGISTRY_PASSWORD}`

const CONFIG_FETCH_EXAMPLE = `- name: Fetch Config
  uses: buz/config-fetch@1.1.0
  with:
    url: \${secrets.CONFIG_BUNDLE_URL}
    dest: config`

// Only an existing secret can retain its original deprecated kind.
const editingLegacyKind = computed(() => {
  const kind = normalizeSecretKind(editing.value?.kind)
  return kind === 'generic' ? null : kind
})

const valuePlaceholder = computed(() => {
  if (editing.value && !editing.value.reencrypt_required) {
    return 'Leave empty to keep current value'
  }
  if (formKind.value === 'registry') return 'Paste registry credentials JSON… (Deprecated kind)'
  if (formKind.value === 'aliyun_oss_auth') return 'Paste Aliyun OSS credentials JSON… (Deprecated kind)'
  return 'Paste secret value…'
})

function openCreate() {
  editing.value = null
  formName.value = ''
  formValue.value = ''
  // Default to the `default` team (matching YAML `team:` omission) so secrets
  // created here line up with YAML-sourced pipelines without a manual team pick.
  const def = teams.value.find((t) => t.slug === 'default')
  const preferred = auth.preferredTeamId
  formTeamId.value = def?.id
    ?? (teams.value.some((t) => t.id === preferred) ? preferred : (teams.value[0]?.id || ''))
  formTeamShared.value = false
  formKind.value = 'generic'
  showValue.value = false
  showDialog.value = true
}

function openEdit(s: Secret) {
  editing.value = s
  formName.value = s.name
  formValue.value = ''
  formTeamId.value = s.team_id || ''
  formTeamShared.value = !!s.team_shared
  formKind.value = normalizeSecretKind(s.kind)
  showValue.value = false
  showDialog.value = true
}

/** Force the stored value to UPPER_SNAKE_CASE on every keystroke, so the value
 *  and its visual rendering (CSS `uppercase`) always agree. */
function onNameInput(e: Event) {
  formName.value = (e.target as HTMLInputElement).value.toUpperCase()
}

async function handleSave() {
  if (!formTeamId.value) {
    await dialog.alert('Team is required')
    return
  }
  if (!editing.value) {
    // Normalize to UPPER_SNAKE_CASE FIRST, then validate the normalized value —
    // otherwise a mixed-case input like "OSS_demo" is rejected before the
    // auto-uppercase on save ever runs. Round-trip the normalized name back
    // into the form so the user sees exactly what will be saved.
    const name = formName.value.trim().toUpperCase()
    const nameErr = validateSecretName(name)
    if (nameErr) {
      await dialog.alert(nameErr)
      return
    }
    formName.value = name
  }
  if (editing.value?.reencrypt_required && !formValue.value) {
    await dialog.alert('This secret must be re-saved with a new value (plaintext storage is no longer accepted).')
    return
  }
  if (!editing.value && !formValue.value) {
    await dialog.alert('Value is required')
    return
  }
  saving.value = true
  try {
    if (editing.value) {
      const target = editing.value
      const res = await secretsApi.update(editing.value.id, {
        value: formValue.value,
        team_id: formTeamId.value,
        team_shared: formTeamShared.value,
        kind: formKind.value,
      })
      showDialog.value = false
      await loadSecrets()
      const stale = res.github_binds_stale ?? 0
      if (stale > 0 && auth.isPlatformAdmin) {
        const go = await dialog.confirm(
          `${stale} GitHub bind${stale === 1 ? '' : 's'} marked as needs push (inherit only; custom overrides unchanged). Push now?`,
          { title: 'GitHub sync', confirmLabel: 'Push now', cancelLabel: 'Later' },
        )
        if (go) {
          const refreshed = secrets.value.find((s) => s.id === target.id) || target
          openGithub(refreshed)
        }
      }
    } else {
      await secretsApi.create({
        name: formName.value.trim().toUpperCase(),
        value: formValue.value,
        team_id: formTeamId.value,
        team_shared: formTeamShared.value,
        kind: formKind.value,
      })
      showDialog.value = false
      await loadSecrets()
    }
  } catch (e: any) {
    // Show the backend's message (e.g. validation failures) instead of letting
    // the unhandled rejection crash the page via ErrorBoundary.
    await dialog.alert(e?.response?.data?.message || e?.message || 'Failed to save secret')
  } finally {
    saving.value = false
  }
}

async function handleDelete(s: Secret) {
  if (!(await dialog.confirm(`Delete secret "${s.name}"? This cannot be undone.`, {
    title: 'Delete secret',
    confirmLabel: 'Delete',
    danger: true,
  }))) return
  try {
    await secretsApi.delete(s.id)
    await loadSecrets()
  } catch (e: any) {
    await dialog.alert(e?.response?.data?.message || e?.message || 'Failed to delete secret')
  }
}

async function loadSecrets() {
  try {
    const [s, t] = await Promise.all([secretsApi.list(), teamsApi.list()])
    secrets.value = s
    teams.value = t
  } catch {
    // ignore
  } finally {
    loading.value = false
  }
}

onMounted(loadSecrets)
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      title="Secrets"
      :description="SECRETS_DESCRIPTION"
    >
      <template #actions>
        <UiButton size="sm" @click="openCreate"><Plus class="h-4 w-4" /> Add Secret</UiButton>
      </template>
    </PageHeader>

    <div v-if="loading" class="text-center py-12 text-muted-foreground text-sm">Loading…</div>
    <div v-else class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
      <!-- Filter bar -->
      <div class="px-5 py-3 border-b border-border/30 flex items-center gap-3 flex-wrap">
        <input
          v-model="searchQuery"
          type="text"
          placeholder="Search secrets…"
          class="w-full max-w-44 rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50 focus:ring-1 focus:ring-primary/10"
        />
        <select v-model="filterTeam" class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50">
          <option value="">All teams</option>
          <option v-for="t in teams" :key="t.id" :value="t.id">{{ t.name }}</option>
        </select>
        <select v-model="filterGithub" class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50" title="Filter by GitHub sync status">
          <option value="">All GitHub status</option>
          <option value="needs_push">Needs push</option>
        </select>
        <span v-if="filteredSecrets.length !== secrets.length" class="text-[10px] text-muted-foreground ml-auto">
          {{ filteredSecrets.length }} of {{ secrets.length }}
        </span>
      </div>
      <template v-if="filteredSecrets.length > 0">
        <div class="overflow-x-auto">
          <table class="w-full text-[13px]">
            <thead>
              <tr class="border-b border-border/30 bg-muted/10">
                <th class="text-left px-4 py-2.5 w-44 min-w-[9rem]">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('name')">
                    Name
                    <span class="font-mono text-xs">{{ sortIndicator('name') }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5 min-w-[20rem] w-[42%]">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('value')">
                    Value
                    <span class="font-mono text-xs">{{ sortIndicator('value') }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('team')">
                    Team
                    <span class="font-mono text-xs">{{ sortIndicator('team') }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('team_shared')">
                    Share
                    <span class="font-mono text-xs">{{ sortIndicator('team_shared') }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('github_status')">
                    GitHub Sync
                    <span class="font-mono text-xs">{{ sortIndicator('github_status') }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('updated_at')">
                    Updated
                    <span class="font-mono text-xs">{{ sortIndicator('updated_at') }}</span>
                  </button>
                </th>
                <th class="text-right px-5 py-2.5 w-20" />
              </tr>
            </thead>
            <tbody class="divide-y divide-border/20">
              <tr v-for="s in pageItems" :key="s.id" class="hover:bg-muted/5 transition-colors">
                <td class="px-4 py-3 w-44 min-w-[9rem]">
                  <div class="flex flex-col gap-1 min-w-0">
                    <span
                      class="font-mono text-[12px] font-medium leading-snug text-foreground/90 break-all"
                      :title="s.name"
                    >{{ s.name }}</span>
                    <span
                      v-if="s.kind === 'registry'"
                      class="inline-flex w-fit items-center rounded-md bg-sky-500/10 px-1.5 py-0.5 text-[10px] font-medium text-sky-700 dark:text-sky-400"
                      title="Deprecated registry credentials kind; retained for existing secrets only"
                    >Registry (Deprecated)</span>
                    <span
                      v-else-if="s.kind === 'aliyun_oss_auth'"
                      class="inline-flex w-fit items-center rounded-md bg-orange-500/10 px-1.5 py-0.5 text-[10px] font-medium text-orange-700 dark:text-orange-400"
                      title="Deprecated Aliyun OSS credentials kind; retained for existing secrets only"
                    >Aliyun OSS (Deprecated)</span>
                    <span
                      v-else
                      class="inline-flex w-fit items-center rounded-md bg-muted/60 px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground"
                      title="Generic secret for plugin with, step env, or config.render templates"
                    >Generic</span>
                  </div>
                </td>
                <td class="px-5 py-3 min-w-[20rem] w-[42%]">
                  <div class="flex flex-col gap-1 min-w-0">
                    <code class="mono-chip text-xs select-all break-all whitespace-pre-wrap">{{ s.value }}</code>
                    <span
                      v-if="s.reencrypt_required"
                      class="text-[10px] font-medium text-amber-600"
                    >
                      Re-save required (legacy plaintext is no longer accepted)
                    </span>
                  </div>
                </td>
                <td class="px-5 py-3 text-xs text-muted-foreground whitespace-nowrap">{{ s.owner_group || s.team_id || '—' }}</td>
                <td class="px-5 py-3 text-xs whitespace-nowrap">
                  <span
                    v-if="s.team_shared"
                    class="inline-flex items-center rounded-md bg-emerald-500/10 px-1.5 py-0.5 text-[10px] font-medium text-emerald-700 dark:text-emerald-400"
                    title="Same-team pipelines auto-receive this secret"
                  >Team share</span>
                  <span v-else class="text-muted-foreground/70" title="Requires explicit bind or step reference">Explicit</span>
                </td>
                <td class="px-5 py-3">
                  <GitHubBindChips
                    v-if="s.github_binds && s.github_binds.length > 0"
                    :binds="s.github_binds"
                    @open="openGithub(s)"
                  />
                  <span v-else class="text-muted-foreground/50" title="No GitHub repo bound">—</span>
                </td>
                <td class="px-5 py-3 text-xs text-muted-foreground whitespace-nowrap" :title="s.updated_at || ''">{{ formatLocalDateTime(s.updated_at) }}</td>
                <td class="px-5 py-3 text-right whitespace-nowrap">
                  <div class="flex items-center justify-end gap-1">
                    <UiButton variant="ghost" size="sm" @click="openGithub(s)" title="GitHub Actions sync"><GitBranch class="h-3.5 w-3.5" /></UiButton>
                    <UiButton variant="ghost" size="sm" @click="openEdit(s)" title="Edit"><Pencil class="h-3.5 w-3.5" /></UiButton>
                    <UiButton variant="ghost" size="sm" @click="handleDelete(s)" title="Delete"><Trash2 class="h-3.5 w-3.5 text-red-500" /></UiButton>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <TablePagination v-model:page="page" v-model:pageSize="pageSize" :total="total" :page-count="pageCount" :range-label="rangeLabel" />
      </template>
      <div v-else class="flex flex-col items-center justify-center py-16 text-muted-foreground">
        <Shield class="h-10 w-10 mb-3 text-muted-foreground/20" />
        <p class="text-sm">{{ secrets.length === 0 ? 'No secrets configured yet' : 'No secrets in the selected team' }}</p>
        <p class="text-xs mt-1 text-muted-foreground/60">
          Create generic secrets here, then reference <code class="mono-chip text-[10px]">{{ SECRET_REFERENCE }}</code>
          in plugin <code class="mono-chip text-[10px]">with</code>, step <code class="mono-chip text-[10px]">env</code>,
          or <code class="mono-chip text-[10px]">config.render</code> templates.
          Enable <span class="font-medium text-foreground">Team share</span> only when every same-team pipeline needs access.
        </p>
      </div>
    </div>

    <!-- Dialog -->
    <Teleport to="body">
      <div v-if="showDialog" class="fixed inset-0 z-50 flex items-start justify-center overflow-y-auto py-8">
        <div class="fixed inset-0 bg-black/30 backdrop-blur-sm" @click="showDialog = false" />
        <div class="relative z-10 w-full max-w-md mx-4 my-auto rounded-2xl border border-border/50 bg-card shadow-2xl flex flex-col max-h-[min(85vh,calc(100vh-4rem))]">
          <h3 class="shrink-0 text-[15px] font-semibold px-6 pt-6 pb-4">{{ editing ? 'Update Secret' : 'New Secret' }}</h3>
          <div class="flex-1 min-h-0 overflow-y-auto px-6 space-y-4">
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Name <span class="text-red-500">*</span></label>
              <input
                :value="formName"
                :disabled="!!editing"
                @input="onNameInput"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono uppercase outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20 disabled:bg-muted/30 disabled:cursor-not-allowed"
                placeholder="POSTGRES_PASSWORD"
              />
              <p class="text-[10px] text-muted-foreground mt-1 leading-relaxed">
                UPPER_SNAKE_CASE — reference <code class="mono-chip text-[10px]">{{ SECRET_REFERENCE }}</code>
                in plugin <code class="mono-chip text-[10px]">with</code>, step <code class="mono-chip text-[10px]">env</code>,
                or <code class="mono-chip text-[10px]">config.render</code> templates. Auto-uppercased on save.
              </p>
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Team <span class="text-red-500">*</span></label>
              <select
                v-model="formTeamId"
                class="w-full appearance-none rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20 cursor-pointer"
              >
                <option disabled value="">Select a team</option>
                <option v-for="t in teams" :key="t.id" :value="t.id">{{ t.name }} ({{ t.slug }})</option>
              </select>
              <p class="text-[10px] text-muted-foreground mt-1">Ownership and CRUD boundary. Does not auto-inject unless Team share is enabled.</p>
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Kind <span class="text-red-500">*</span></label>
              <select
                v-model="formKind"
                class="w-full appearance-none rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20 cursor-pointer"
              >
                <option value="generic">Generic — plugin with / env / config.render</option>
                <option v-if="editingLegacyKind === 'registry'" value="registry">Registry (Deprecated)</option>
                <option v-if="editingLegacyKind === 'aliyun_oss_auth'" value="aliyun_oss_auth">Aliyun OSS auth (Deprecated)</option>
              </select>
              <p class="text-[10px] text-muted-foreground mt-1 leading-relaxed">
                New secrets are Generic. Store one credential or URL per secret and reference it in plugin inputs.
              </p>
              <p v-if="editingLegacyKind" class="text-[10px] text-amber-600 mt-1 leading-relaxed">
                Deprecated kind: saving keeps the original kind unless you explicitly select Generic.
                Do not pass old credentials JSON directly as a plugin password. Save the username and password
                separately as Generic secrets for docker-login. For config-fetch, save the full download URL
                (including any signed query parameters) as a Generic secret instead of the old OSS credentials JSON.
              </p>
            </div>
            <div>
              <label class="flex items-start gap-2.5 cursor-pointer">
                <input
                  v-model="formTeamShared"
                  type="checkbox"
                  class="mt-0.5 h-3.5 w-3.5 rounded border-border accent-primary cursor-pointer"
                />
                <span>
                  <span class="block text-[12px] font-medium text-foreground">Team share</span>
                  <span class="block text-[10px] text-muted-foreground mt-0.5 leading-relaxed">
                    When enabled, every pipeline in this team auto-receives this secret as an env var at job claim.
                    Leave off for sensitive credentials — reference <code class="mono-chip text-[10px]">{{ SECRET_REFERENCE }}</code>
                    explicitly in plugin <code class="mono-chip text-[10px]">with</code> or step
                    <code class="mono-chip text-[10px]">env</code>. For <code class="mono-chip text-[10px]">config.render</code>
                    templates, authorize only the needed names with step <code class="mono-chip text-[10px]">secrets: [NAME]</code>.
                  </span>
                </span>
              </label>
            </div>
            <div class="pb-1">
              <label class="block text-[12px] font-medium text-foreground mb-1">
                Value <span class="text-red-500">*</span>
                <span v-if="editing && !editing.reencrypt_required" class="text-muted-foreground font-normal ml-1">(leave empty to keep current)</span>
                <span v-else-if="editing?.reencrypt_required" class="text-amber-600 font-normal ml-1">(required — re-encrypt)</span>
              </label>
              <div class="relative">
                <textarea
                  v-model="formValue"
                  rows="6"
                  spellcheck="false"
                  class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 pr-14 text-sm font-mono outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20 resize-y min-h-[7.5rem]"
                  :style="showValue ? '' : '-webkit-text-security: disc'"
                  :placeholder="valuePlaceholder"
                />
                <button type="button" class="absolute right-2 top-2 text-[11px] text-muted-foreground hover:text-foreground transition-colors cursor-pointer" @click="showValue = !showValue">
                  {{ showValue ? 'Hide' : 'Show' }}
                </button>
              </div>
              <p v-if="editing?.reencrypt_required" class="text-[10px] text-amber-600 mt-1.5">
                This row is not valid ciphertext. Enter the secret again to encrypt it at rest.
              </p>
              <p class="text-[10px] text-muted-foreground mt-1.5 leading-relaxed">
                Stored encrypted at rest. List view shows a partial redaction only.
                Save a single value, then reference <code class="mono-chip text-[10px]">{{ SECRET_REFERENCE }}</code>
                in plugin <code class="mono-chip text-[10px]">with</code>, step <code class="mono-chip text-[10px]">env</code>,
                or <code class="mono-chip text-[10px]">config.render</code> templates.
              </p>
              <details class="mt-3 rounded-lg border border-border/50 bg-muted/30">
                <summary class="cursor-pointer select-none px-3 py-2.5 text-[11px] font-medium text-foreground">
                  Private registry — docker-login plugin
                </summary>
                <div class="px-3 pb-2.5 space-y-2 border-t border-border/40 pt-2">
                  <p class="text-[10px] text-muted-foreground leading-relaxed">
                    Save the actual registry login username as REGISTRY_USERNAME and its password as REGISTRY_PASSWORD,
                    each in a separate Generic secret. For ACR Personal, use the login name and fixed password from
                    the ACR console. The plugin password input is the password itself, not a credentials JSON object.
                    Run this login step before pulling or pushing private images.
                  </p>
                  <pre class="text-[10px] font-mono text-muted-foreground whitespace-pre-wrap break-all leading-relaxed">{{ DOCKER_LOGIN_EXAMPLE }}</pre>
                </div>
              </details>
              <details class="mt-2 rounded-lg border border-border/50 bg-muted/30">
                <summary class="cursor-pointer select-none px-3 py-2.5 text-[11px] font-medium text-foreground">
                  Config download — config-fetch plugin
                </summary>
                <div class="px-3 pb-2.5 space-y-2 border-t border-border/40 pt-2">
                  <p class="text-[10px] text-muted-foreground leading-relaxed">
                    Save the complete .tar.gz bundle download URL as CONFIG_BUNDLE_URL in a Generic secret.
                    For private OSS, include the full presigned URL with its filename and signed query parameters;
                    keep it valid for the job. Do not use the old OSS credentials JSON as the URL.
                    Fetch into the job-workspace-relative config directory, then use the same
                    <code class="mono-chip text-[10px]">config.dest</code> for a later
                    <code class="mono-chip text-[10px]">config.render</code> step on the same target.
                  </p>
                  <pre class="text-[10px] font-mono text-muted-foreground whitespace-pre-wrap break-all leading-relaxed">{{ CONFIG_FETCH_EXAMPLE }}</pre>
                </div>
              </details>
            </div>
          </div>
          <div class="shrink-0 flex items-center justify-end gap-3 px-6 py-5 border-t border-border/30">
            <UiButton variant="ghost" size="sm" @click="showDialog = false" :disabled="saving">Cancel</UiButton>
            <UiButton size="sm" @click="handleSave" :disabled="saving">{{ saving ? 'Saving…' : (editing ? 'Update' : 'Create') }}</UiButton>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- GitHub Sync bindings dialog -->
    <GitHubBindDialog
      v-model:show="showGithubDialog"
      resource-kind="secret"
      :resource-id="githubTarget?.id || ''"
      :resource-name="githubTarget?.name || ''"
      :can-manage="githubTarget ? canManageSecret(githubTarget) : false"
      @changed="onGithubChanged"
    />
  </div>
</template>
