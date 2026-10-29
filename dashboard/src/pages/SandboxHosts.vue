<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import {
  Boxes,
  Check,
  Copy,
  History,
  KeyRound,
  Plus,
  RefreshCw,
  ShieldCheck,
  Trash2,
  X,
} from '@lucide/vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import UiButton from '@/components/ui/UiButton.vue'
import { useAuthStore } from '@/stores/auth'
import { useRouter } from 'vue-router'
import { useAppDialog } from '@/composables/useAppDialog'
import {
  sandboxHostsApi,
  type ConfigVersion,
  type PendingChange,
  type ResourceGrant,
  type SandboxHost,
  type SandboxHostDocument,
} from '@/api/sandboxHosts'
import { teamsApi, type Team } from '@/api/teams'

const auth = useAuthStore()
const router = useRouter()
const dialog = useAppDialog()

type Tab = 'hosts' | 'changes' | 'grants'
const tab = ref<Tab>('hosts')
const loading = ref(true)
const hosts = ref<SandboxHost[]>([])
const changes = ref<PendingChange[]>([])
const grants = ref<ResourceGrant[]>([])
const teams = ref<Team[]>([])

// ── enroll ──
const showEnroll = ref(false)
const enrollHostId = ref('')
const enrollInstanceId = ref('')
const enrollSlots = ref(0)
const creating = ref(false)
const issuedToken = ref('')
const tokenCopied = ref(false)

// ── config editor ──
const selected = ref<SandboxHost | null>(null)
const configText = ref('')
const configError = ref('')
const configWarnings = ref<string[]>([])
const savingConfig = ref(false)
const versions = ref<ConfigVersion[]>([])
const versionsLoading = ref(false)

// ── grants form ──
const grantTeamId = ref('')
const grantProfile = ref('')

const canManage = computed(() => auth.isPlatformAdmin || auth.isOps)

function parseDocument(text: string): SandboxHostDocument | null {
  try {
    const parsed = JSON.parse(text)
    if (!parsed || typeof parsed !== 'object' || !parsed.profiles || !parsed.network) {
      configError.value = 'document must be an object with profiles and network'
      return null
    }
    configError.value = ''
    return parsed as SandboxHostDocument
  } catch (e) {
    configError.value = `invalid JSON: ${e instanceof Error ? e.message : String(e)}`
    return null
  }
}

async function load() {
  if (!canManage.value) {
    router.replace('/dashboard')
    return
  }
  loading.value = true
  try {
    const [hostList, changeList, grantList, teamList] = await Promise.all([
      sandboxHostsApi.list(),
      sandboxHostsApi.changes('pending'),
      sandboxHostsApi.grants(),
      teamsApi.list().catch(() => [] as Team[]),
    ])
    hosts.value = hostList
    changes.value = changeList
    grants.value = grantList
    teams.value = teamList
    if (selected.value) {
      const fresh = hostList.find(
        (h) => h.host_id === selected.value!.host_id && h.instance_id === selected.value!.instance_id,
      )
      if (fresh) selected.value = fresh
    }
  } finally {
    loading.value = false
  }
}

onMounted(load)

function openEnroll() {
  enrollHostId.value = ''
  enrollInstanceId.value = ''
  enrollSlots.value = 0
  issuedToken.value = ''
  tokenCopied.value = false
  showEnroll.value = true
}

async function handleEnroll() {
  const hostId = enrollHostId.value.trim()
  const instanceId = enrollInstanceId.value.trim()
  if (!hostId || !instanceId) return
  creating.value = true
  try {
    const { token } = await sandboxHostsApi.enroll(hostId, instanceId, Number(enrollSlots.value) || 0)
    issuedToken.value = token
    await load()
  } catch (e) {
    console.error(e)
    await dialog.alert('Failed to enroll sandbox host')
  } finally {
    creating.value = false
  }
}

async function copyToken() {
  const { copyText } = await import('@/utils/copyText')
  await copyText(issuedToken.value)
  tokenCopied.value = true
}

async function openConfig(host: SandboxHost) {
  selected.value = host
  configWarnings.value = []
  if (host.document) {
    configText.value = JSON.stringify(host.document, null, 2)
    configError.value = ''
  } else {
    configText.value = JSON.stringify(
      {
        profiles: { default: { memory: '2g', cpu: '2', pids: 256, slots: 1 } },
        tiers: {},
        network: 'bridge',
      },
      null,
      2,
    )
  }
  void loadVersions(host)
}

async function loadVersions(host: SandboxHost) {
  versionsLoading.value = true
  try {
    versions.value = await sandboxHostsApi.versions(host.host_id, host.instance_id)
  } catch {
    versions.value = []
  } finally {
    versionsLoading.value = false
  }
}

async function validateConfig() {
  if (!selected.value) return
  const doc = parseDocument(configText.value)
  if (!doc) return
  const result = await sandboxHostsApi.validate(selected.value.host_id, selected.value.instance_id, doc)
  configError.value = (result.problems ?? []).join('; ')
  configWarnings.value = result.warnings ?? []
  if (!configError.value && !configWarnings.value.length) {
    await dialog.alert('Document is valid')
  }
}

async function saveConfig() {
  if (!selected.value) return
  const doc = parseDocument(configText.value)
  if (!doc) return
  savingConfig.value = true
  try {
    const result = await sandboxHostsApi.putConfig(
      selected.value.host_id,
      selected.value.instance_id,
      doc,
      selected.value.desired_version,
    )
    configWarnings.value = result.warnings ?? []
    if (result.pending) {
      await dialog.alert(
        `Approval is enabled — change queued as ${result.change_id}. A different ops user must approve it.`,
      )
    } else {
      await dialog.alert(`Configuration published as version ${result.version}`)
    }
    await load()
    const fresh = hosts.value.find(
      (h) =>
        h.host_id === selected.value!.host_id && h.instance_id === selected.value!.instance_id,
    )
    if (fresh) {
      selected.value = fresh
      await loadVersions(fresh)
    }
  } catch (e) {
    console.error(e)
    await dialog.alert('Failed to publish configuration (stale If-Match?) — reload and retry')
    await load()
  } finally {
    savingConfig.value = false
  }
}

async function rollbackTo(version: number) {
  if (!selected.value) return
  const ok = await dialog.confirm(`Restore version ${version} as a new version?`)
  if (!ok) return
  try {
    const result = await sandboxHostsApi.rollback(selected.value.host_id, selected.value.instance_id, version)
    await dialog.alert(`Rolled back — published as version ${result.version}`)
    await load()
    await loadVersions(selected.value)
  } catch (e) {
    console.error(e)
    await dialog.alert('Rollback failed')
  }
}

async function rotateToken(host: SandboxHost) {
  const ok = await dialog.confirm(
    `Rotate the token for ${host.instance_id}? The old token stops working after the next fetch attempt.`,
  )
  if (!ok) return
  try {
    const { token } = await sandboxHostsApi.rotateToken(host.host_id, host.instance_id)
    issuedToken.value = token
    tokenCopied.value = false
    showEnroll.value = true
    enrollHostId.value = host.host_id
    enrollInstanceId.value = host.instance_id
  } catch (e) {
    console.error(e)
    await dialog.alert('Token rotation failed')
  }
}

async function decide(change: PendingChange, approve: boolean) {
  try {
    await sandboxHostsApi.decideChange(change.id, approve)
    await load()
    if (selected.value) await loadVersions(selected.value)
  } catch (e) {
    console.error(e)
    await dialog.alert(approve ? 'Approval failed (self-approval?)' : 'Rejection failed')
  }
}

async function addGrant() {
  const teamId = grantTeamId.value
  const profile = grantProfile.value.trim()
  if (!teamId || !profile) return
  try {
    await sandboxHostsApi.grant(teamId, profile)
    grantProfile.value = ''
    await load()
  } catch (e) {
    console.error(e)
    await dialog.alert('Grant failed (profile name / team?)')
  }
}

async function removeGrant(grant: ResourceGrant) {
  const ok = await dialog.confirm(`Revoke '${grant.profile}' from ${grant.team_name}? Queued jobs lose it at claim.`)
  if (!ok) return
  try {
    await sandboxHostsApi.revokeGrant(grant.team_id, grant.profile)
    await load()
  } catch (e) {
    console.error(e)
    await dialog.alert('Revoke failed')
  }
}

</script>

<template>
  <div class="space-y-6">
    <PageHeader title="Sandbox Hosts" description="Manager-managed policy for buz-sandboxd: resource profiles, tier images, and team grants.">
      <template #actions>
        <UiButton size="sm" @click="openEnroll"><Plus class="h-4 w-4" /> Enroll host</UiButton>
        <UiButton size="sm" variant="ghost" :loading="loading" @click="load">
          <RefreshCw class="h-4 w-4" />
        </UiButton>
      </template>
    </PageHeader>

    <div class="flex gap-1 border-b border-border/60">
      <button
        v-for="t in (['hosts', 'changes', 'grants'] as const)"
        :key="t"
        class="px-4 py-2 text-sm capitalize border-b-2 -mb-px"
        :class="tab === t ? 'border-primary text-foreground font-medium' : 'border-transparent text-muted-foreground hover:text-foreground'"
        @click="tab = t"
      >
        {{ t }}
        <span v-if="t === 'changes' && changes.length" class="ml-1 text-xs rounded-full bg-amber-500/15 text-amber-600 px-1.5">
          {{ changes.length }}
        </span>
      </button>
    </div>

    <!-- ── Hosts ── -->
    <div v-if="tab === 'hosts'" class="space-y-4">
      <div v-if="loading" class="surface-card p-8 text-center type-meta">Loading…</div>
      <div v-else-if="hosts.length === 0" class="rounded-xl border border-border/50 bg-card p-16 text-center text-muted-foreground">
        <Boxes class="h-10 w-10 mx-auto mb-3 text-muted-foreground/20" />
        <p class="text-sm">No sandbox hosts enrolled</p>
        <p class="text-xs mt-1">Enroll a (host_id, instance_id) pair, then point buz-sandboxd's BUZ_MANAGER_URL + token file at this Manager.</p>
      </div>
      <div v-else class="grid gap-4 lg:grid-cols-2">
        <div
          v-for="host in hosts"
          :key="host.host_id + '/' + host.instance_id"
          class="rounded-xl border bg-card p-4 space-y-3 cursor-pointer hover:border-primary/40"
          :class="selected?.instance_id === host.instance_id ? 'border-primary/60' : 'border-border/50'"
          @click="openConfig(host)"
        >
          <div class="flex items-center justify-between">
            <div>
              <p class="font-medium text-sm">{{ host.host_id }} / {{ host.instance_id }}</p>
              <p class="type-meta">budget {{ host.max_resource_slots || '∞' }} slots · applied v{{ host.applied_version }} of desired v{{ host.desired_version }}</p>
            </div>
            <StatusBadge :status="host.status" />
          </div>
          <div v-if="host.applied_version < host.desired_version" class="text-xs text-amber-600">
            Pending apply — dispatch keeps using v{{ host.applied_version }} until sandboxd catches up.
          </div>
          <div v-if="host.last_fetch_error" class="text-xs text-destructive truncate">{{ host.last_fetch_error }}</div>
          <div class="flex items-center gap-2 text-xs text-muted-foreground">
            <span v-if="host.attached_runners?.length">{{ host.attached_runners.length }} runner(s)</span>
            <span v-if="host.applied_at">· applied {{ host.applied_at }}</span>
          </div>
          <div class="flex gap-2" @click.stop>
            <UiButton size="sm" variant="ghost" @click="rotateToken(host)"><KeyRound class="h-3.5 w-3.5" /> Rotate token</UiButton>
            <UiButton size="sm" variant="ghost" @click="openConfig(host)"><History class="h-3.5 w-3.5" /> Config</UiButton>
          </div>
        </div>
      </div>

      <!-- config editor -->
      <div v-if="selected" class="rounded-xl border border-border/50 bg-card p-4 space-y-3">
        <div class="flex items-center justify-between">
          <h3 class="text-sm font-medium">
            Desired document — {{ selected.host_id }}/{{ selected.instance_id }}
            <span class="type-meta">(next version = max(desired, applied) + 1)</span>
          </h3>
          <UiButton size="sm" variant="ghost" @click="selected = null"><X class="h-4 w-4" /></UiButton>
        </div>
        <textarea
          v-model="configText"
          rows="16"
          spellcheck="false"
          class="w-full font-mono text-xs rounded-lg border border-border/60 bg-background p-3"
        ></textarea>
        <p v-if="configError" class="text-xs text-destructive">{{ configError }}</p>
        <ul v-if="configWarnings.length" class="text-xs text-amber-600 list-disc list-inside">
          <li v-for="w in configWarnings" :key="w">{{ w }}</li>
        </ul>
        <div class="flex gap-2">
          <UiButton size="sm" variant="ghost" :disabled="savingConfig" @click="validateConfig">
            <ShieldCheck class="h-3.5 w-3.5" /> Validate
          </UiButton>
          <UiButton size="sm" :loading="savingConfig" @click="saveConfig">Publish</UiButton>
        </div>

        <div>
          <h4 class="text-xs font-medium text-muted-foreground mb-2">Version history</h4>
          <div v-if="versionsLoading" class="type-meta">Loading…</div>
          <ul v-else class="space-y-1">
            <li v-for="v in versions" :key="v.version" class="flex items-center justify-between text-xs border rounded-lg px-3 py-2">
              <span>
                v{{ v.version }}
                <span class="type-meta ml-2">{{ v.created_by }} · {{ v.created_at }}</span>
              </span>
              <UiButton v-if="v.version !== selected?.desired_version" size="sm" variant="ghost" @click="rollbackTo(v.version)">
                Restore
              </UiButton>
            </li>
          </ul>
        </div>
      </div>
    </div>

    <!-- ── Pending changes ── -->
    <div v-if="tab === 'changes'" class="space-y-3">
      <div v-if="changes.length === 0" class="rounded-xl border border-border/50 bg-card p-12 text-center text-sm text-muted-foreground">
        No pending configuration changes.
      </div>
      <div v-for="change in changes" :key="change.id" class="rounded-xl border border-border/50 bg-card p-4 space-y-2">
        <div class="flex items-center justify-between">
          <div>
            <p class="text-sm font-medium">{{ change.host_id }}/{{ change.instance_id }}</p>
            <p class="type-meta">requested by {{ change.requested_by }} · {{ change.created_at }}</p>
          </div>
          <div class="flex gap-2">
            <UiButton size="sm" @click="decide(change, true)"><Check class="h-3.5 w-3.5" /> Approve</UiButton>
            <UiButton size="sm" variant="ghost" @click="decide(change, false)"><X class="h-3.5 w-3.5" /> Reject</UiButton>
          </div>
        </div>
        <pre class="text-xs bg-muted/40 rounded-lg p-3 overflow-auto max-h-48">{{ JSON.stringify(change.after_document, null, 2) }}</pre>
      </div>
    </div>

    <!-- ── Grants ── -->
    <div v-if="tab === 'grants'" class="space-y-4">
      <div class="rounded-xl border border-border/50 bg-card p-4 space-y-3">
        <p class="text-sm">
          <span class="font-medium">default</span> is implicitly available to every team. Any other
          profile needs an explicit grant; revoking one fails queued jobs at claim.
        </p>
        <div class="flex gap-2">
          <select v-model="grantTeamId" class="text-sm rounded-lg border border-border/60 bg-background px-3 py-2">
            <option value="" disabled>Select team…</option>
            <option v-for="team in teams" :key="team.id" :value="team.id">{{ team.name }}</option>
          </select>
          <input
            v-model="grantProfile"
            placeholder="profile name, e.g. large"
            class="text-sm rounded-lg border border-border/60 bg-background px-3 py-2 flex-1"
          />
          <UiButton size="sm" @click="addGrant">Grant</UiButton>
        </div>
      </div>
      <div class="rounded-xl border border-border/50 bg-card overflow-hidden">
        <table class="w-full text-sm">
          <thead class="bg-muted/40 text-left type-meta">
            <tr><th class="px-4 py-2">Team</th><th class="px-4 py-2">Profile</th><th class="px-4 py-2">Granted</th><th class="px-4 py-2"></th></tr>
          </thead>
          <tbody>
            <tr v-if="grants.length === 0">
              <td colspan="4" class="px-4 py-8 text-center text-muted-foreground">No grants — every team can use default only.</td>
            </tr>
            <tr v-for="grant in grants" :key="grant.team_id + grant.profile" class="border-t border-border/40">
              <td class="px-4 py-2">{{ grant.team_name }}</td>
              <td class="px-4 py-2 font-mono text-xs">{{ grant.profile }}</td>
              <td class="px-4 py-2 type-meta">{{ grant.created_at }}</td>
              <td class="px-4 py-2 text-right">
                <UiButton size="sm" variant="ghost" @click="removeGrant(grant)"><Trash2 class="h-3.5 w-3.5" /></UiButton>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <!-- ── enroll / token dialog ── -->
    <div v-if="showEnroll" class="fixed inset-0 z-50 flex items-center justify-center bg-black/40" @click.self="showEnroll = false">
      <div class="bg-card rounded-xl border border-border/60 p-6 w-full max-w-md space-y-4 shadow-lg">
        <template v-if="!issuedToken">
          <h3 class="text-sm font-medium">Enroll sandbox host</h3>
          <input v-model="enrollHostId" placeholder="host_id (BUZ_HOST_ID on the machine)" class="w-full text-sm rounded-lg border border-border/60 bg-background px-3 py-2" />
          <input v-model="enrollInstanceId" placeholder="instance_id (BUZ_SANDBOXD_INSTANCE_ID)" class="w-full text-sm rounded-lg border border-border/60 bg-background px-3 py-2" />
          <input v-model="enrollSlots" type="number" min="0" placeholder="max_resource_slots (0 = unset)" class="w-full text-sm rounded-lg border border-border/60 bg-background px-3 py-2" />
          <div class="flex justify-end gap-2">
            <UiButton size="sm" variant="ghost" @click="showEnroll = false">Cancel</UiButton>
            <UiButton size="sm" :loading="creating" :disabled="!enrollHostId.trim() || !enrollInstanceId.trim()" @click="handleEnroll">Enroll</UiButton>
          </div>
        </template>
        <template v-else>
          <h3 class="text-sm font-medium flex items-center gap-2"><KeyRound class="h-4 w-4" /> Token issued once</h3>
          <p class="text-xs text-muted-foreground">
            Store it in the sandboxd token file (mode 0400) pointed at by BUZ_SANDBOXD_TOKEN_FILE.
            Only its SHA-256 is stored — this value cannot be shown again.
          </p>
          <code class="block text-xs break-all bg-muted/40 rounded-lg p-3">{{ issuedToken }}</code>
          <div class="flex justify-end gap-2">
            <UiButton size="sm" variant="ghost" @click="copyToken">
              <Copy class="h-3.5 w-3.5" /> {{ tokenCopied ? 'Copied' : 'Copy' }}
            </UiButton>
            <UiButton size="sm" @click="showEnroll = false">Done</UiButton>
          </div>
        </template>
      </div>
    </div>
  </div>
</template>
