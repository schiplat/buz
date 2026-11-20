<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import TablePagination from '@/components/ui/TablePagination.vue'
import { BookOpen, Clock, Layers, Plus, Trash2, Users } from '@lucide/vue'
import { usersApi, type User } from '@/api/users'
import { teamsApi, type Team, type TeamMember } from '@/api/teams'
import { oidcIdentitiesApi, type PendingIdentity } from '@/api/oidcIdentities'
import { useAuthStore } from '@/stores/auth'
import { useClientSort } from '@/composables/useClientSort'
import { useClientPagination } from '@/composables/useClientPagination'
import { useAppDialog } from '@/composables/useAppDialog'

const DOCS_URL = 'https://buz.docs.sconts.com/docs/01-architecture/platform/rbac/'
const ROLES = ['owner', 'maintainer', 'developer', 'viewer'] as const

const auth = useAuthStore()
const dialog = useAppDialog()
const isAdmin = computed(() => auth.isPlatformAdmin)

const users = ref<User[]>([])
const teams = ref<Team[]>([])
const teamDetails = ref<Record<string, Team>>({})
const pending = ref<PendingIdentity[]>([])
const loading = ref(true)
const acting = ref(false)

/** Right-hand panel: all accounts, an ungrouped bucket (admin), or one team. */
type Selection =
  | { kind: 'all' }
  | { kind: 'ungrouped' }
  | { kind: 'team'; id: string }
const selection = ref<Selection>({ kind: 'all' })

const isAllSelected = computed(() => selection.value.kind === 'all')
const isUngroupedSelected = computed(() => selection.value.kind === 'ungrouped')
function isTeamSelected(id: string) {
  return selection.value.kind === 'team' && selection.value.id === id
}

const selectedTeam = computed<Team | null>(() =>
  selection.value.kind === 'team' ? (teamDetails.value[selection.value.id] ?? null) : null,
)

function membersOf(teamId: string): TeamMember[] {
  return teamDetails.value[teamId]?.members ?? []
}

/** user_id → the teams they belong to (and their role in each). */
const membershipsByUser = computed(() => {
  const map = new Map<string, Array<{ team: Team; role: string }>>()
  for (const t of teams.value) {
    for (const m of membersOf(t.id)) {
      const list = map.get(m.user_id) ?? []
      list.push({ team: t, role: m.role })
      map.set(m.user_id, list)
    }
  }
  return map
})

/** Active accounts that hold no team — the bucket that needs attention first. */
const ungroupedUsers = computed(() =>
  users.value.filter((u) => !membershipsByUser.value.has(u.id)),
)

/** Subset an admin can act on: platform admins do not need a team; disabled
 *  accounts cannot be added (the API only accepts active users). */
const actionableUngrouped = computed(() =>
  ungroupedUsers.value.filter((u) => u.is_active && !u.is_platform_admin),
)

function canManageTeam(team: Team | null): boolean {
  return !!team && (isAdmin.value || team.my_role === 'owner')
}

function nodeClass(active: boolean) {
  return [
    'flex w-full items-center gap-2 rounded-lg px-2.5 py-1.5 text-[13px] transition-colors',
    active
      ? 'bg-muted font-medium text-foreground'
      : 'text-muted-foreground hover:bg-muted/40 hover:text-foreground',
  ]
}

const searchQuery = ref('')
const filteredUsers = computed(() => {
  const q = searchQuery.value.trim().toLowerCase()
  if (!q) return users.value
  return users.value.filter((u) => {
    const memberships = (membershipsByUser.value.get(u.id) ?? [])
      .map((m) => `${m.team.name} ${m.role}`)
      .join(' ')
    return [
      u.username,
      u.display_name,
      u.is_platform_admin ? 'platform admin' : 'member',
      u.is_super_admin ? 'super admin' : '',
      u.is_active ? 'active' : 'disabled',
      memberships,
    ]
      .filter(Boolean)
      .join(' ')
      .toLowerCase()
      .includes(q)
  })
})

const { sorted, toggleSort, sortIndicator } = useClientSort(filteredUsers, {
  initialKey: 'username',
  initialDir: 'asc',
  getValue: (row, key) => {
    switch (key) {
      case 'username': return row.username
      case 'role': return row.is_super_admin ? 2 : row.is_platform_admin ? 1 : 0
      case 'status': return row.is_active ? 1 : 0
      default: return ''
    }
  },
})

const { page, pageSize, pageCount, total, pageItems, rangeLabel } = useClientPagination(sorted)

// ── loading ──────────────────────────────────────────────────────────────────

async function load() {
  try {
    const [teamList, userList, pendingList] = await Promise.all([
      teamsApi.list(),
      isAdmin.value ? usersApi.list() : Promise.resolve<User[]>([]),
      isAdmin.value
        ? oidcIdentitiesApi.listPending().catch(() => [] as PendingIdentity[])
        : Promise.resolve<PendingIdentity[]>([]),
    ])
    teams.value = teamList
    users.value = userList
    pending.value = pendingList

    const details = await Promise.all(
      teamList.map((t) => teamsApi.get(t.id).catch(() => null)),
    )
    const map: Record<string, Team> = {}
    for (const detail of details) {
      if (detail) map[detail.id] = detail
    }
    teamDetails.value = map

    // Keep the selection pointing at something that still exists.
    if (selection.value.kind === 'team' && !map[selection.value.id]) {
      selection.value = !isAdmin.value && teamList[0]
        ? { kind: 'team', id: teamList[0].id }
        : isAdmin.value
          ? { kind: 'all' }
          : { kind: 'team', id: '' }
    } else if (!isAdmin.value && selection.value.kind !== 'team') {
      selection.value = teamList[0] ? { kind: 'team', id: teamList[0].id } : { kind: 'team', id: '' }
    }
  } catch {
    // keep whatever loaded; the empty/loading states cover the rest
  } finally {
    loading.value = false
  }
}

// ── SSO identity approval queue ──────────────────────────────────────────────

const bindTarget = ref<Record<string, string>>({}) // pending id → existing user id ('' = create a new user)
const bindUsername = ref<Record<string, string>>({})
const bindDisablePassword = ref<Record<string, boolean>>({})

async function approveIdentity(item: PendingIdentity) {
  const existingId = bindTarget.value[item.id] ?? ''
  const disablePassword = bindDisablePassword.value[item.id] ?? false
  acting.value = true
  try {
    const result = await oidcIdentitiesApi.bind(item.id, {
      user_id: existingId || undefined,
      create_username: existingId ? undefined : bindUsername.value[item.id]?.trim() || undefined,
      disable_local_password: existingId ? disablePassword : undefined,
    })
    await dialog.alert(
      `Identity approved: ${result.created_user ? `created user "${result.username}"` : `bound to "${result.username}"`}${result.password_login_disabled ? ' (local password disabled)' : ''}.`,
    )
    bindTarget.value[item.id] = ''
    bindUsername.value[item.id] = ''
    bindDisablePassword.value[item.id] = false
    await load()
  } catch (e: any) {
    await dialog.alert(e?.message || 'Approval failed')
  } finally {
    acting.value = false
  }
}

async function dismissIdentity(item: PendingIdentity) {
  if (!(await dialog.confirm(`Dismiss identity "${item.email || item.subject}"? A later sign-in attempt will re-add it.`, {
    title: 'Dismiss identity',
    confirmLabel: 'Dismiss',
    danger: true,
  }))) return
  acting.value = true
  try {
    await oidcIdentitiesApi.dismiss(item.id)
    await load()
  } finally {
    acting.value = false
  }
}

// ── account actions (platform admin) ─────────────────────────────────────────

const showCreateUser = ref(false)
const formUsername = ref('')
const formPassword = ref('')
const formDisplay = ref('')
const formAdmin = ref(false)
const formSuper = ref(false)
const saving = ref(false)

async function createUser() {
  if (!formUsername.value.trim() || !formPassword.value) {
    await dialog.alert('Username and password are required')
    return
  }
  saving.value = true
  try {
    await usersApi.create({
      username: formUsername.value.trim(),
      password: formPassword.value,
      display_name: formDisplay.value.trim() || undefined,
      is_platform_admin: formAdmin.value,
      is_super_admin: auth.isSuperAdmin ? formSuper.value : undefined,
    })
    showCreateUser.value = false
    formUsername.value = ''
    formPassword.value = ''
    formDisplay.value = ''
    formAdmin.value = false
    formSuper.value = false
    await load()
  } finally {
    saving.value = false
  }
}

async function toggleActive(u: User) {
  acting.value = true
  try {
    await usersApi.update(u.id, { is_active: !u.is_active })
    await load()
  } finally {
    acting.value = false
  }
}

async function toggleSuper(u: User) {
  const action = u.is_super_admin ? 'demote from super admin' : 'promote to super admin'
  if (!(await dialog.confirm(`${action[0].toUpperCase() + action.slice(1)} "${u.username}"?`, {
    title: 'Change role',
    confirmLabel: 'Confirm',
    danger: true,
  }))) return
  acting.value = true
  try {
    await usersApi.update(u.id, { is_super_admin: !u.is_super_admin })
    await load()
  } finally {
    acting.value = false
  }
}

async function deleteUser(u: User) {
  if (!(await dialog.confirm(`Delete user "${u.username}"?`, {
    title: 'Delete user',
    confirmLabel: 'Delete',
    danger: true,
  }))) return
  acting.value = true
  try {
    await usersApi.delete(u.id)
    await load()
  } finally {
    acting.value = false
  }
}

// ── team actions ─────────────────────────────────────────────────────────────

const showCreateTeam = ref(false)
const formName = ref('')
const formSlug = ref('')
const formDesc = ref('')

async function createTeam() {
  if (!formName.value.trim() || !formSlug.value.trim()) {
    await dialog.alert('Name and slug are required')
    return
  }
  saving.value = true
  try {
    await teamsApi.create({
      name: formName.value.trim(),
      slug: formSlug.value.trim().toLowerCase(),
      description: formDesc.value.trim() || undefined,
    })
    showCreateTeam.value = false
    formName.value = ''
    formSlug.value = ''
    formDesc.value = ''
    await load()
  } finally {
    saving.value = false
  }
}

async function removeTeam(teamId: string, name: string) {
  if (!(await dialog.confirm(`Delete team "${name}"?`, {
    title: 'Delete team',
    confirmLabel: 'Delete',
    danger: true,
  }))) return
  acting.value = true
  try {
    await teamsApi.delete(teamId)
    await load()
  } catch (e: any) {
    // The delete guard reports pipelines/secrets/hosts/runners in a 409 —
    // surface it instead of failing silently.
    await dialog.alert(e?.response?.data?.message || e?.message || 'Failed to delete team')
  } finally {
    acting.value = false
  }
}

const memberUserId = ref('')
const memberRole = ref('developer')

// Ungrouped bucket: per-account team + role pickers (direct grant).
const grantTeam = ref<Record<string, string>>({})
const grantRole = ref<Record<string, string>>({})

async function grantAccess(u: User) {
  const teamId = grantTeam.value[u.id]
  if (!teamId) return
  acting.value = true
  try {
    await teamsApi.upsertMember(teamId, {
      user_id: u.id,
      role: grantRole.value[u.id] || 'developer',
    })
    grantTeam.value[u.id] = ''
    await load()
  } finally {
    acting.value = false
  }
}

const addableUsers = computed(() => {
  const team = selectedTeam.value
  if (!team) return []
  const ids = new Set(membersOf(team.id).map((m) => m.user_id))
  return users.value.filter((u) => u.is_active && !ids.has(u.id))
})

async function addMember() {
  const team = selectedTeam.value
  if (!team || !memberUserId.value) return
  acting.value = true
  try {
    await teamsApi.upsertMember(team.id, { user_id: memberUserId.value, role: memberRole.value })
    memberUserId.value = ''
    await load()
  } finally {
    acting.value = false
  }
}

async function changeRole(teamId: string, m: TeamMember, role: string) {
  if (!role || role === m.role) return
  acting.value = true
  try {
    await teamsApi.upsertMember(teamId, { user_id: m.user_id, role })
    await load()
  } finally {
    acting.value = false
  }
}

function onRoleChange(teamId: string, m: TeamMember, event: Event) {
  const role = (event.target as HTMLSelectElement).value
  void changeRole(teamId, m, role)
}

async function removeMember(teamId: string, m: TeamMember) {
  if (!(await dialog.confirm(`Remove ${m.username} from this team?`, {
    title: 'Remove member',
    confirmLabel: 'Remove',
    danger: true,
  }))) return
  acting.value = true
  try {
    await teamsApi.removeMember(teamId, m.user_id)
    await load()
  } finally {
    acting.value = false
  }
}

onMounted(load)
</script>

<template>
  <div class="space-y-5">
    <PageHeader
      title="Users &amp; teams"
      description="Who signs in, and what they may do. SSO is configured under Settings → SSO."
    >
      <template #actions>
        <UiButton as="a" :href="DOCS_URL" target="_blank" variant="ghost" size="sm">
          <BookOpen class="h-3.5 w-3.5" />
          Docs
        </UiButton>
      </template>
    </PageHeader>

    <!-- first-use orientation: identity & access in three steps (only when no
         pending approvals and no accounts are waiting for a team) -->
    <div
      v-if="isAdmin && pending.length === 0 && actionableUngrouped.length === 0"
      class="flex flex-wrap items-center gap-x-2 gap-y-1 surface-card p-3.5 text-xs text-muted-foreground"
    >
      <span class="font-medium text-foreground">Identity &amp; access in three steps:</span>
      <span>
        ①
        <RouterLink class="underline decoration-border underline-offset-2 hover:text-foreground" to="/settings/sso">Connect SSO</RouterLink>
        — colleagues sign in with your provider.
      </span>
      <span>
        ② Accounts appear here on first sign-in <span class="font-mono">SSO</span>
        (created automatically, or queued below for approval).
      </span>
      <span>③ Grant what they may do — select a team on the left and add them with a role.</span>
    </div>

    <div v-if="loading" class="text-center py-12 text-muted-foreground text-sm">Loading…</div>

    <template v-else>
      <!-- SSO identity approval queue -->
      <div v-if="isAdmin && pending.length > 0" class="notice-warning overflow-hidden">
        <div class="px-5 py-3 border-b ">
          <p class="text-[13px] font-medium">Pending SSO identities</p>
          <p class="text-[11px] text-muted-foreground mt-0.5">
            Sign-ins refused because auto-provision is off. Approve to bind to an account (or create a new one), or dismiss.
          </p>
        </div>
        <div class="divide-y divide-border/20">
          <div v-for="item in pending" :key="item.id" class="px-5 py-3 space-y-2">
            <div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-[13px]">
              <span class="font-mono text-[12px] text-foreground/90">{{ item.email || item.subject }}</span>
              <span v-if="item.display_name" class="text-muted-foreground text-xs">{{ item.display_name }}</span>
              <span class="text-[11px] text-muted-foreground">{{ item.attempts }} attempt{{ item.attempts === 1 ? '' : 's' }} · last {{ item.last_seen_at?.slice(0, 19).replace('T', ' ') }}</span>
            </div>
            <div class="flex flex-wrap items-center gap-2 text-xs">
              <select
                v-model="bindTarget[item.id]"
                class="field-select-sm"
              >
                <option value="">Create new user</option>
                <option v-for="u in users.filter((u) => u.auth_source === 'local' && u.is_active)" :key="u.id" :value="u.id">
                  Bind to {{ u.username }}
                </option>
              </select>
              <input
                v-if="!(bindTarget[item.id])"
                v-model="bindUsername[item.id]"
                type="text"
                :placeholder="`Username (default: ${(item.email || 'sso-user').split('@')[0]})`"
                class="w-52 rounded-xl border border-border bg-background px-2 py-1 text-xs"
              />
              <label v-if="bindTarget[item.id]" class="flex items-center gap-1.5 text-[11px] text-muted-foreground">
                <input v-model="bindDisablePassword[item.id]" type="checkbox" class="rounded-sm" />
                Also disable local password
              </label>
              <div class="ml-auto flex gap-2">
                <UiButton size="sm" variant="ghost" :disabled="acting" @click="dismissIdentity(item)">Dismiss</UiButton>
                <UiButton size="sm" :disabled="acting" @click="approveIdentity(item)">Approve</UiButton>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- accounts that can sign in but hold no team: the admin action cue -->
      <div
        v-if="isAdmin && pending.length === 0 && actionableUngrouped.length > 0"
        class="flex flex-wrap items-center gap-x-3 gap-y-2 notice-warning px-4 py-3 text-xs"
      >
        <Clock class="h-3.5 w-3.5 shrink-0 text-warning" />
        <span class="font-medium text-foreground">
          {{ actionableUngrouped.length }} account{{ actionableUngrouped.length === 1 ? '' : 's' }}
          {{ actionableUngrouped.length === 1 ? 'has' : 'have' }} no team yet
        </span>
        <span class="text-muted-foreground">Assign a role so they can use Buz.</span>
        <UiButton size="sm" class="ml-auto" @click="selection = { kind: 'ungrouped' }">Review</UiButton>
      </div>

      <div class="grid items-start gap-5 lg:grid-cols-[300px_minmax(0,1fr)]">
        <!-- ── left: teams tree ─────────────────────────────────────────── -->
        <aside class="overflow-hidden surface-card">
          <div v-if="!isAdmin" class="flex items-center justify-between px-4 pt-3.5 pb-2.5">
            <h2 class="text-[13.5px] font-semibold">My teams</h2>
          </div>
          <div v-else class="px-4 pt-3.5 pb-1">
            <p class="type-eyebrow">Accounts</p>
          </div>
          <div class="px-2 pb-2.5">
            <button
              v-if="isAdmin"
              type="button"
              :class="nodeClass(isAllSelected)"
              @click="selection = { kind: 'all' }"
            >
              <Users class="h-4 w-4 shrink-0" />
              <span class="min-w-0 flex-1 truncate text-left">All accounts</span>
              <span class="text-[11px] text-muted-foreground">{{ users.length }}</span>
            </button>

            <button
              v-if="isAdmin"
              type="button"
              :class="nodeClass(isUngroupedSelected)"
              @click="selection = { kind: 'ungrouped' }"
            >
              <Clock class="h-4 w-4 shrink-0" />
              <span class="min-w-0 flex-1 truncate text-left">Ungrouped</span>
              <span class="rounded-lg bg-warning/10 px-1.5 py-0.5 text-[10px] font-medium text-warning">
                {{ ungroupedUsers.length }} no team
              </span>
            </button>

            <div v-if="isAdmin" class="mt-3 flex items-center justify-between px-2.5 pb-1">
              <p class="type-eyebrow">Teams</p>
              <UiButton variant="ghost" size="sm" @click="showCreateTeam = true">
                <Plus class="h-3.5 w-3.5" /> New team
              </UiButton>
            </div>

            <button
              v-for="t in teams"
              :key="t.id"
              type="button"
              :class="nodeClass(isTeamSelected(t.id))"
              @click="selection = { kind: 'team', id: t.id }"
            >
              <Layers class="h-4 w-4 shrink-0" />
              <span class="min-w-0 flex-1 truncate text-left">{{ t.name }}</span>
              <span class="text-[11px] text-muted-foreground">
                {{ membersOf(t.id).length }}<template v-if="t.my_role"> · {{ t.my_role }}</template>
              </span>
            </button>

            <p v-if="teams.length === 0" class="px-2.5 pt-2 pb-1 text-xs text-muted-foreground">
              {{ isAdmin ? 'No teams yet — create one to start granting access.' : 'You are not in a team yet.' }}
            </p>
          </div>
          <p class="border-t border-border/40 px-3.5 py-2.5 text-[11px] leading-relaxed text-muted-foreground">
            <template v-if="isAdmin">
              SSO accounts sign in through the provider and hold no team until granted. Teams own pipelines and secrets — roles: Owner / Maintainer / Developer / Viewer.
            </template>
            <template v-else>
              You see only teams you belong to. Ask a team owner for access to another team.
            </template>
          </p>
        </aside>

        <!-- ── right: selected view ─────────────────────────────────────── -->
        <section
          v-if="isAllSelected && isAdmin"
          class="min-w-0 overflow-hidden surface-card"
        >
          <div class="flex flex-wrap items-center gap-3 px-4 pt-3.5 pb-3">
            <div class="min-w-0">
              <h2 class="text-[13.5px] font-semibold">All accounts</h2>
              <p class="mt-0.5 text-[11.5px] text-muted-foreground">Platform-wide accounts and their sign-in origin. Team membership lives in the tree.</p>
            </div>
            <input
              v-model="searchQuery"
              type="text"
              placeholder="Search accounts…"
              class="ml-auto w-48 rounded-xl border border-border bg-background px-2.5 py-1.5 text-xs outline-hidden transition-colors focus-visible:border-foreground/35 focus-visible:ring-2 focus-visible:ring-ring/30"
            />
            <UiButton size="sm" @click="showCreateUser = true"><Plus class="h-3.5 w-3.5" /> New user</UiButton>
          </div>

          <div v-if="users.length === 0" class="flex flex-col items-center justify-center py-16 text-muted-foreground">
            <Users class="h-10 w-10 mb-3 text-muted-foreground/20" />
            <p class="text-sm">No accounts</p>
          </div>
          <template v-else>
            <div class="overflow-x-auto">
              <table class="w-full text-[13px]">
                <thead>
                  <tr class="border-y border-border/40 bg-muted/40">
                    <th class="text-left px-4 py-2.5">
                      <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('username')">
                        Account
                        <span class="font-mono text-xs">{{ sortIndicator('username') }}</span>
                      </button>
                    </th>
                    <th class="text-left px-4 py-2.5 text-[11px] uppercase text-muted-foreground">Teams</th>
                    <th class="text-left px-4 py-2.5">
                      <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('role')">
                        Role
                        <span class="font-mono text-xs">{{ sortIndicator('role') }}</span>
                      </button>
                    </th>
                    <th class="text-left px-4 py-2.5">
                      <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('status')">
                        Status
                        <span class="font-mono text-xs">{{ sortIndicator('status') }}</span>
                      </button>
                    </th>
                    <th class="text-right px-4 py-2.5 w-32" />
                  </tr>
                </thead>
                <tbody class="divide-y divide-border/20">
                  <tr v-for="u in pageItems" :key="u.id" class="hover:bg-muted/40">
                    <td class="px-4 py-2.5">
                      <span class="font-mono text-[12px] font-medium leading-snug text-foreground/90 break-all" :title="u.username">{{ u.username }}</span>
                      <span
                        v-if="u.auth_source === 'oidc'"
                        class="ml-2 inline-flex items-center rounded-lg bg-primary/10 px-1.5 py-0.5 text-[10px] font-medium text-primary align-middle"
                        title="Signs in through SSO; no local password"
                      >SSO</span>
                      <span v-if="u.display_name" class="ml-2 text-xs text-muted-foreground">{{ u.display_name }}</span>
                    </td>
                    <td class="px-4 py-2.5 text-xs text-muted-foreground">
                      <template v-if="(membershipsByUser.get(u.id) ?? []).length">
                        <span v-for="(m, i) in membershipsByUser.get(u.id)" :key="m.team.id">
                          {{ i ? ', ' : '' }}{{ m.team.name }} · {{ m.role }}
                        </span>
                      </template>
                      <span v-else>no team</span>
                    </td>
                    <td class="px-4 py-2.5 text-xs">
                      <span v-if="u.is_super_admin" class="font-semibold text-warning">super admin</span>
                      <span v-else-if="u.is_platform_admin">platform admin</span>
                      <span v-else>member</span>
                    </td>
                    <td class="px-4 py-2.5 text-xs">{{ u.is_active ? 'active' : 'disabled' }}</td>
                    <td class="px-4 py-2.5 text-right space-x-1">
                      <UiButton v-if="auth.isSuperAdmin && u.id !== auth.userId" variant="ghost" size="sm" :disabled="acting" @click="toggleSuper(u)">
                        {{ u.is_super_admin ? 'Demote' : 'Promote' }}
                      </UiButton>
                      <UiButton variant="ghost" size="sm" :disabled="acting" @click="toggleActive(u)">
                        {{ u.is_active ? 'Disable' : 'Enable' }}
                      </UiButton>
                      <UiButton variant="ghost" size="sm" :disabled="acting" @click="deleteUser(u)">
                        <Trash2 class="h-3.5 w-3.5 text-destructive" />
                      </UiButton>
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
            <TablePagination v-model:page="page" v-model:pageSize="pageSize" :total="total" :page-count="pageCount" :range-label="rangeLabel" />
          </template>
        </section>

        <section
          v-else-if="selectedTeam"
          class="min-w-0 overflow-hidden surface-card"
        >
          <div class="flex flex-wrap items-start gap-3 px-4 pt-3.5 pb-3">
            <div class="min-w-0">
              <h2 class="text-[13.5px] font-semibold">{{ selectedTeam.name }} · members</h2>
              <p class="mt-0.5 font-mono text-[11.5px] text-muted-foreground">
                {{ selectedTeam.slug }}<template v-if="selectedTeam.description"> · {{ selectedTeam.description }}</template>
              </p>
            </div>
            <div class="ml-auto flex items-center gap-2">
              <span v-if="selectedTeam.my_role" class="rounded-lg bg-muted px-2 py-0.5 text-[10.5px] font-medium text-muted-foreground">
                your role: {{ selectedTeam.my_role }}
              </span>
              <UiButton
                v-if="canManageTeam(selectedTeam)"
                variant="ghost"
                size="sm"
                title="Delete team"
                :disabled="acting"
                @click="removeTeam(selectedTeam.id, selectedTeam.name)"
              >
                <Trash2 class="h-3.5 w-3.5 text-destructive" />
              </UiButton>
            </div>
          </div>

          <div class="overflow-x-auto">
            <table class="w-full text-[13px]">
              <thead>
                <tr class="border-y border-border/40 bg-muted/40">
                  <th class="text-left px-4 py-2.5 text-[11px] uppercase text-muted-foreground">Member</th>
                  <th class="text-left px-4 py-2.5 text-[11px] uppercase text-muted-foreground">Role</th>
                  <th class="text-right px-4 py-2.5 w-24" />
                </tr>
              </thead>
              <tbody class="divide-y divide-border/20">
                <tr v-for="m in membersOf(selectedTeam.id)" :key="m.user_id" class="hover:bg-muted/40">
                  <td class="px-4 py-2.5">
                    <span class="font-mono text-[12px] font-medium text-foreground/90">{{ m.username }}</span>
                    <span v-if="m.user_id === auth.userId" class="ml-2 text-[10px] text-muted-foreground">you</span>
                    <span v-if="m.display_name" class="ml-2 text-xs text-muted-foreground">{{ m.display_name }}</span>
                  </td>
                  <td class="px-4 py-2.5 text-xs">
                    <select
                      v-if="canManageTeam(selectedTeam)"
                      :value="m.role"
                      class="field-select-sm"
                      :disabled="acting"
                      @change="onRoleChange(selectedTeam.id, m, $event)"
                    >
                      <option v-for="r in ROLES" :key="r" :value="r">{{ r }}</option>
                    </select>
                    <span v-else>{{ m.role }}</span>
                  </td>
                  <td class="px-4 py-2.5 text-right">
                    <UiButton
                      v-if="canManageTeam(selectedTeam)"
                      variant="ghost"
                      size="sm"
                      :title="`Remove ${m.username}`"
                      :disabled="acting"
                      @click="removeMember(selectedTeam.id, m)"
                    >
                      <Trash2 class="h-3.5 w-3.5 text-destructive" />
                    </UiButton>
                  </td>
                </tr>
                <tr v-if="membersOf(selectedTeam.id).length === 0">
                  <td colspan="3" class="px-4 py-8 text-center text-xs text-muted-foreground">No members yet.</td>
                </tr>
              </tbody>
            </table>
          </div>

          <div
            v-if="isAdmin && canManageTeam(selectedTeam)"
            class="flex flex-wrap items-center gap-2 border-t border-border/40 px-4 py-3"
          >
            <select v-model="memberUserId" class="field-select-sm min-w-40">
              <option value="">Select account</option>
              <option v-for="u in addableUsers" :key="u.id" :value="u.id">{{ u.username }}</option>
            </select>
            <select v-model="memberRole" class="field-select-sm">
              <option v-for="r in ROLES" :key="r" :value="r">{{ r }}</option>
            </select>
            <UiButton size="sm" :disabled="acting || !memberUserId" @click="addMember">Add</UiButton>
            <span v-if="addableUsers.length === 0" class="text-[11px] text-muted-foreground">Every active account is already in this team.</span>
          </div>

          <p class="border-t border-border/40 px-4 py-2.5 text-[11px] leading-relaxed text-muted-foreground">
            <template v-if="canManageTeam(selectedTeam)">Roles are managed here; changes apply immediately.</template>
            <template v-else>Roles are managed by the team owner.</template>
          </p>
        </section>

        <section
          v-else-if="isUngroupedSelected && isAdmin"
          class="min-w-0 overflow-hidden surface-card"
        >
          <div class="px-4 pt-3.5 pb-3">
            <h2 class="text-[13.5px] font-semibold">Ungrouped</h2>
            <p class="mt-0.5 text-[11.5px] text-muted-foreground">Accounts with no team membership. Assign a team and role right here, or manage the team from the tree.</p>
          </div>
          <div v-if="ungroupedUsers.length === 0" class="flex flex-col items-center justify-center py-14 text-muted-foreground">
            <Users class="h-9 w-9 mb-3 text-muted-foreground/20" />
            <p class="text-sm">Every account belongs to a team.</p>
          </div>
          <template v-else>
            <table class="w-full text-[13px]">
              <thead>
                <tr class="border-y border-border/40 bg-muted/40">
                  <th class="text-left px-4 py-2.5 text-[11px] uppercase text-muted-foreground">Account</th>
                  <th class="text-left px-4 py-2.5 text-[11px] uppercase text-muted-foreground">Status</th>
                  <th class="text-left px-4 py-2.5 text-[11px] uppercase text-muted-foreground">Assign team</th>
                </tr>
              </thead>
              <tbody class="divide-y divide-border/20">
                <tr v-for="u in ungroupedUsers" :key="u.id" class="hover:bg-muted/40">
                  <td class="px-4 py-2.5">
                    <span class="font-mono text-[12px] font-medium text-foreground/90">{{ u.username }}</span>
                    <span
                      v-if="u.auth_source === 'oidc'"
                      class="ml-2 inline-flex items-center rounded-lg bg-primary/10 px-1.5 py-0.5 text-[10px] font-medium text-primary align-middle"
                    >SSO</span>
                  </td>
                  <td class="px-4 py-2.5 text-xs">{{ u.is_active ? 'active' : 'disabled' }}</td>
                  <td class="px-4 py-2.5">
                    <div v-if="u.is_active && !u.is_platform_admin" class="flex flex-wrap items-center gap-1.5">
                      <select
                        v-model="grantTeam[u.id]"
                        class="field-select-sm"
                        :disabled="acting || teams.length === 0"
                      >
                        <option value="">Select team</option>
                        <option v-for="t in teams" :key="t.id" :value="t.id">{{ t.name }}</option>
                      </select>
                      <select
                        v-model="grantRole[u.id]"
                        class="field-select-sm"
                        :disabled="acting || teams.length === 0"
                      >
                        <option v-for="r in ROLES" :key="r" :value="r">{{ r }}</option>
                      </select>
                      <UiButton size="sm" :disabled="acting || !grantTeam[u.id]" @click="grantAccess(u)">Grant</UiButton>
                    </div>
                    <span v-else class="text-[11px] text-muted-foreground">—</span>
                  </td>
                </tr>
              </tbody>
            </table>
          </template>
        </section>

        <section
          v-else
          class="flex min-w-0 flex-col items-center justify-center surface-card py-16 text-center"
        >
          <Users class="h-10 w-10 mb-3 text-muted-foreground/20" />
          <p class="text-sm font-medium text-foreground">No team yet</p>
          <p class="mt-1 max-w-70 text-xs text-muted-foreground">
            Ask a team owner to add you, or a platform admin to create a team.
          </p>
        </section>
      </div>
    </template>

    <!-- ── modals ──────────────────────────────────────────────────────── -->
    <Teleport to="body">
      <div v-if="showCreateUser" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-xs" @click="showCreateUser = false" />
        <div class="relative z-10 w-full max-w-md mx-4 surface-modal p-6">
          <h3 class="text-[15px] font-semibold mb-5">New user</h3>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium mb-1">Username <span class="text-destructive">*</span></label>
              <input v-model="formUsername" class="w-full rounded-xl border border-border bg-background px-3 py-2 text-sm" />
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Password <span class="text-destructive">*</span></label>
              <input v-model="formPassword" type="password" class="w-full rounded-xl border border-border bg-background px-3 py-2 text-sm" />
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Display name</label>
              <input v-model="formDisplay" class="w-full rounded-xl border border-border bg-background px-3 py-2 text-sm" />
            </div>
            <label class="flex items-center gap-2 text-sm">
              <input v-model="formAdmin" type="checkbox" class="rounded-sm" />
              Platform admin
            </label>
            <label v-if="auth.isSuperAdmin" class="flex items-center gap-2 text-sm">
              <input v-model="formSuper" type="checkbox" class="rounded-sm" />
              Super admin (sees all API tokens &amp; audit logs)
            </label>
          </div>
          <div class="flex justify-end gap-3 mt-6 pt-5 border-t border-border/40">
            <UiButton variant="ghost" size="sm" @click="showCreateUser = false">Cancel</UiButton>
            <UiButton size="sm" :disabled="saving" @click="createUser">{{ saving ? 'Creating…' : 'Create' }}</UiButton>
          </div>
        </div>
      </div>
    </Teleport>

    <Teleport to="body">
      <div v-if="showCreateTeam" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-xs" @click="showCreateTeam = false" />
        <div class="relative z-10 w-full max-w-md mx-4 surface-modal p-6">
          <h3 class="text-[15px] font-semibold mb-5">New team</h3>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium mb-1">Name <span class="text-destructive">*</span></label>
              <input v-model="formName" class="w-full rounded-xl border border-border bg-background px-3 py-2 text-sm" placeholder="Backend" />
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Slug <span class="text-destructive">*</span></label>
              <input v-model="formSlug" class="w-full rounded-xl border border-border bg-background px-3 py-2 text-sm font-mono" placeholder="backend" />
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Description</label>
              <input v-model="formDesc" class="w-full rounded-xl border border-border bg-background px-3 py-2 text-sm" />
            </div>
          </div>
          <div class="flex justify-end gap-3 mt-6 pt-5 border-t border-border/40">
            <UiButton variant="ghost" size="sm" @click="showCreateTeam = false">Cancel</UiButton>
            <UiButton size="sm" :disabled="saving" @click="createTeam">{{ saving ? 'Creating…' : 'Create' }}</UiButton>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>
