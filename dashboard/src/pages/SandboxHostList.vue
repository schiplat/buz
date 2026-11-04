<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch } from 'vue'
import {
  AlertTriangle,
  Boxes,
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  KeyRound,
  Plus,
  RefreshCw,
  Search,
  ShieldCheck,
  Trash2,
  X,
} from '@lucide/vue'
import UiButton from '@/components/ui/UiButton.vue'
import { useAppDialog } from '@/composables/useAppDialog'
import {
  sandboxHostsApi,
  type ConfigVersion,
  type PendingChange,
  type ResourceProfile,
  type SandboxHost,
  type SandboxHostDocument,
} from '@/api/sandboxHosts'
import { teamsApi, type Team } from '@/api/teams'
import { useRoute } from 'vue-router'

const route = useRoute()
import { type ResourceGrant } from '@/api/sandboxHosts'
import { formatLocalShort } from '@/utils/time'

const dialog = useAppDialog()

/** Raw API timestamps rendered in the viewer's local time. */
function formatDateTime(ts: string | null): string {
  return formatLocalShort(ts, '—')
}

const loading = ref(true)
const hosts = ref<SandboxHost[]>([])
const changes = ref<PendingChange[]>([])
const teams = ref<Team[]>([])
const grants = ref<ResourceGrant[]>([])

// ── profile grants: team-centric rows (fleet-wide allowlist) ──
const grantError = ref('')
const grantSaving = ref(false)
/** Inline "grant profiles" panel: which team row is open, its pending
 *  multi-select and the search text narrowing the checkbox list. */
const grantPanelTeamId = ref('')
const grantPanelPending = ref<Set<string>>(new Set())
const grantPanelSearch = ref('')

/** Grants table keeps one line per team: the first CHIP_PREVIEW chips, then a
 *  "+N more" expander that unfolds the rest in place. */
const CHIP_PREVIEW = 3
const expandedGrantTeams = ref<Set<string>>(new Set())

function toggleGrantsExpanded(teamId: string) {
  const next = new Set(expandedGrantTeams.value)
  if (next.has(teamId)) next.delete(teamId)
  else next.add(teamId)
  expandedGrantTeams.value = next
}

/** Chips visible in a team row: preview slice unless the row is expanded. */
function visibleGrants(teamId: string): ResourceGrant[] {
  const list = teamGrants(teamId)
  return expandedGrantTeams.value.has(teamId) ? list : list.slice(0, CHIP_PREVIEW)
}

const PROFILE_RE = /^[a-z0-9][a-z0-9-]{0,31}$/

/** Profiles offered by each host come straight from the list API, whose
 *  document is already the *effective* one (platform defaults merged) — a
 *  single source of truth shared with dispatch. `default` is the implicit
 *  fallback (alias of `small`) and is never surfaced as an offer. */
function offeredProfilesFor(host: SandboxHost): Set<string> {
  return new Set(
    Object.keys(host.document?.profiles ?? {}).filter((name) => name !== 'default'),
  )
}

/** Size names a host offers, smallest first (slots weight, then memory). */
function offeredProfileNames(host: SandboxHost): string[] {
  return [...offeredProfilesFor(host)].sort(sizeAscending)
}

/** host_ids whose effective document offers this profile. */
function offeredByFor(profile: string): string[] {
  return hosts.value
    .filter((h) => offeredProfilesFor(h).has(profile))
    .map((h) => h.host_id)
    .sort()
}

function isTeamGranted(teamId: string, profile: string): boolean {
  return grants.value.some((g) => g.team_id === teamId && g.profile === profile)
}

/** Grants grouped by team, profiles alphabetical inside each team. */
const grantsByTeam = computed(() => {
  const byTeam = new Map<string, ResourceGrant[]>()
  for (const g of grants.value) {
    const list = byTeam.get(g.team_id) ?? []
    list.push(g)
    byTeam.set(g.team_id, list)
  }
  for (const list of byTeam.values()) {
    list.sort((a, b) => sizeAscending(a.profile, b.profile))
  }
  return byTeam
})

function teamGrants(teamId: string): ResourceGrant[] {
  return grantsByTeam.value.get(teamId) ?? []
}

/** Team picked in the "grant to team" strip — its row opens with the panel. */
const newBindingTeamId = ref('')

/** Binding rows: only teams that currently hold at least one grant, plus any
 *  grant whose team no longer resolves (name falls back) so orphaned grants
 *  stay visible and revocable. Unbound teams live in the grant-to-team picker
 *  instead of occupying rows; the currently-picked team joins so its grant
 *  panel has a row to live in. */
const grantTeamRows = computed(() => {
  const rows = new Map<string, string>()
  const track = (id: string, name: string) => {
    if (!rows.has(id)) rows.set(id, name)
  }
  for (const g of grants.value) {
    const t = teams.value.find((x) => x.id === g.team_id)
    track(g.team_id, t?.name ?? (g.team_name || g.team_id))
  }
  if (newBindingTeamId.value) {
    const t = teams.value.find((x) => x.id === newBindingTeamId.value)
    if (t) track(t.id, t.name)
  }
  return [...rows.entries()]
    .map(([id, name]) => ({ id, name }))
    .sort((a, b) => a.name.localeCompare(b.name))
})

/** Teams without bindings — the grant-to-team picker's candidates. */
const unboundTeams = computed(() => {
  const bound = new Set(grantTeamRows.value.map((r) => r.id))
  return teams.value.filter((t) => !bound.has(t.id))
})

function pickNewBindingTeam() {
  if (newBindingTeamId.value) openGrantPanel(newBindingTeamId.value)
}

/** Panel list narrowed by the search box; pending picks float to the top so
 *  a multi-grant stays scannable while filtering. */
const grantPanelChoices = computed(() => {
  const q = grantPanelSearch.value.trim().toLowerCase()
  const pending = grantPanelPending.value
  return grantableProfileNames.value
    .filter((name) => !q || name.toLowerCase().includes(q))
    .sort((a, b) => {
      const aPend = pending.has(a) ? 0 : 1
      const bPend = pending.has(b) ? 0 : 1
      return aPend - bPend || a.localeCompare(b)
    })
})

function openGrantPanel(teamId: string) {
  grantPanelTeamId.value = teamId
  grantPanelPending.value = new Set()
  grantPanelSearch.value = ''
  grantError.value = ''
}

function closeGrantPanel() {
  grantPanelTeamId.value = ''
  grantPanelPending.value = new Set()
  grantPanelSearch.value = ''
  grantError.value = ''
  newBindingTeamId.value = ''
}

function togglePanelPending(profile: string) {
  const next = new Set(grantPanelPending.value)
  if (next.has(profile)) next.delete(profile)
  else next.add(profile)
  grantPanelPending.value = next
}

/** One action, many grants: POST every pending (team, profile) pair in
 *  order; stop at the first failure and keep the rest pending for retry. */
/** Approvals inbox state: Pending tab = the live queue; History tab =
 *  approved/rejected rows re-fetched on every switch. */
const approvalsTab = ref<'pending' | 'history'>('pending')
const historyRows = ref<PendingChange[]>([])
const historyPage = ref(1)
const historyPageSize = 20
const historyTotal = ref(0)
const historyLoading = ref(false)
const historyTotalPages = computed(() => Math.max(1, Math.ceil(historyTotal.value / historyPageSize)))

async function loadHistory(page = historyPage.value) {
  historyLoading.value = true
  try {
    // The API pages the merged decided view directly; one call per page.
    const res = await sandboxHostsApi.changes('decided', page, historyPageSize)
    historyRows.value = res.changes
    historyTotal.value = res.total
    historyPage.value = page
  } catch (e) { console.error(e) } finally {
    historyLoading.value = false
  }
}

async function switchHistory() {
  approvalsTab.value = 'history'
  if (approvalsTab.value && historyPage.value === 1 && !historyRows.value.length) {
    await loadHistory(1)
  }
}

/** Grant pending changes use host_id='resource-grants' + a grant payload
 *  instead of a host document — surface them as human text in the queue. */
function changeGrantInfo(change: PendingChange): { team_id: string; profile: string; granted: boolean; pipelines?: number; queued_jobs?: number } {
  const doc = change.after_document as unknown as Record<string, unknown>
  return {
    team_id: String(doc?.team_id ?? ''),
    profile: String(doc?.profile ?? ''),
    granted: doc?.granted === true,
    pipelines: typeof doc?.pipelines === 'number' ? doc.pipelines : undefined,
    queued_jobs: typeof doc?.queued_jobs === 'number' ? doc.queued_jobs : undefined,
  }
}

/** Grants awaiting a second-person approval (approval switch on). The
 *  pending change itself appears in the Pending changes panel. */
const grantsPendingHint = ref('')
function noteGrantPending(count: number) {
  if (count > 0) {
    grantsPendingHint.value =
      `${count} grant change${count > 1 ? 's' : ''} queued for approval — see Pending changes.`
  }
}

async function applyPanelGrants() {
  grantError.value = ''
  const teamId = grantPanelTeamId.value
  if (!teamId || !grantPanelPending.value.size) return
  grantSaving.value = true
  let queued = 0
  try {
    for (const profile of grantPanelPending.value) {
      if (isTeamGranted(teamId, profile)) continue
      if (!PROFILE_RE.test(profile)) {
        grantError.value = `Profile name '${profile}' is invalid — expected [a-z0-9][a-z0-9-]{0,31}.`
        return
      }
      try {
        const resp = await sandboxHostsApi.grant(teamId, profile)
        if (resp?.pending) queued += 1
      } catch (e) {
        console.error(e)
        grantError.value = `Failed to grant '${profile}' — remaining picks stay pending for retry.`
        return
      }
    }
    closeGrantPanel()
    await load()
    noteGrantPending(queued)
  } finally {
    grantSaving.value = false
  }
}

async function revokeGrant(grant: ResourceGrant) {
  const ok = await dialog.confirm(
    `Revoke '${grant.profile}' from ${grant.team_name}? Pipelines of this team requesting that profile stop dispatching to sandbox hosts.`,
    { title: 'Revoke grant', confirmLabel: 'Revoke', danger: true },
  )
  if (!ok) return
  try {
    const resp = await sandboxHostsApi.revokeGrant(grant.team_id, grant.profile)
    await load()
    noteGrantPending(resp?.pending ? 1 : 0)
  } catch (e) {
    console.error(e)
    await dialog.alert('Revoke failed')
  }
}

/** Row-level "remove binding": revoke every profile the team holds, one API
 *  call each — same sequential, partial-failure semantics as multi-grant. */
const revokingTeamId = ref('')
async function revokeTeamGrants(team: { id: string; name: string }) {
  const list = teamGrants(team.id)
  if (!list.length || revokingTeamId.value) return
  const ok = await dialog.confirm(
    `Remove all ${list.length} profile grant${list.length > 1 ? 's' : ''} from ${team.name}? Pipelines of this team requesting any of them stop dispatching to sandbox hosts.`,
    { title: 'Remove binding', confirmLabel: 'Remove all', danger: true },
  )
  if (!ok) return
  revokingTeamId.value = team.id
  let queued = 0
  try {
    for (const g of list) {
      try {
        const resp = await sandboxHostsApi.revokeGrant(g.team_id, g.profile)
        if (resp?.pending) queued += 1
      } catch (e) {
        console.error(e)
        await dialog.alert(`Failed to revoke '${g.profile}' — remaining grants stay in place for retry.`)
        return
      }
    }
    await load()
    noteGrantPending(queued)
  } finally {
    revokingTeamId.value = ''
  }
}

/** Every non-default profile name any host effectively offers — the panel's
 *  candidates (platform defaults + host overrides). Ones already fully listed
 *  still appear so an operator can grant them to another team from there. */
const grantableProfileNames = computed(() => {
  const names = new Set<string>()
  for (const host of hosts.value) {
    for (const name of offeredProfilesFor(host)) {
      if (name !== 'default') names.add(name)
    }
  }
  for (const g of grants.value) names.add(g.profile)
  return [...names].sort()
})

/** Deterministic per-team avatar tint (cycled palette). */
const AVATAR_TINTS = ['#6366f1', '#0ea5e9', '#8b5cf6', '#f59e0b', '#10b981', '#f43f5e', '#06b6d4', '#a855f7']
function teamTint(teamId: string): string {
  const idx = teams.value.findIndex((t) => t.id === teamId)
  return AVATAR_TINTS[(idx >= 0 ? idx : teamId.length) % AVATAR_TINTS.length]
}

/** Team display name; falls back to the raw id if teams failed to load. */
function teamName(teamId: string): string {
  return teams.value.find((t) => t.id === teamId)?.name ?? teamId
}

// ── enroll ──
const showEnroll = ref(false)
const enrollHostId = ref('')
const enrollInstanceId = ref('')
const enrollSlots = ref(0)
const creating = ref(false)
const issuedToken = ref('')
const tokenCopied = ref(false)
const snippetCopied = ref(false)
const enrollError = ref('')
const showAdvanced = ref(false)
const instanceEdited = ref(false)

/** Mirrors shared::instance::validate — fail fast in the dialog, not at 400. */
const ENROLL_ID_RE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/

const knownHostIds = computed(() => [...new Set(hosts.value.map((h) => h.host_id))])
const instancesOnHost = computed(() =>
  hosts.value.filter((h) => h.host_id === enrollHostId.value.trim()),
)

/** sandboxd's own default is `sandboxd-<host_id>`; dedupe with -2/-3 so the
 *  generated ID is always free on that host. */
function suggestInstanceId(hostId: string): string {
  if (!hostId) return ''
  const taken = new Set(
    hosts.value.filter((h) => h.host_id === hostId).map((h) => h.instance_id),
  )
  if (!taken.has(`sandboxd-${hostId}`)) return `sandboxd-${hostId}`
  let n = 2
  while (taken.has(`sandboxd-${hostId}-${n}`)) n += 1
  return `sandboxd-${hostId}-${n}`
}

// The instance ID follows the host until the operator overrides it manually.
watch(enrollHostId, (hostId) => {
  if (!instanceEdited.value) enrollInstanceId.value = suggestInstanceId(hostId.trim())
})

/** The Manager is the source of truth: this block pins the enrolled identity
 *  on the machine, so the pair never has to be typed twice. sandboxd appends
 *  /api/v1 to BUZ_MANAGER_URL itself, so the dashboard origin is correct. */
const bootstrapSnippet = computed(() =>
  [
    `BUZ_HOST_ID=${enrollHostId.value.trim()}`,
    `BUZ_SANDBOXD_INSTANCE_ID=${enrollInstanceId.value.trim()}`,
    `BUZ_SANDBOXD_TOKEN=${issuedToken.value}`,
    `BUZ_MANAGER_URL=${window.location.origin}`,
  ].join('\n'),
)

// ── host detail dialog: config form ──
const detailHost = ref<SandboxHost | null>(null)

/** Capacity panel: the host's slot budget. 0 = inherit platform default;
 *  dirty state surfaces explicit Save / Cancel buttons. */
const budgetDraft = ref(0)
const savingBudget = ref(false)
watch(detailHost, (host) => {
  budgetDraft.value = host?.max_resource_slots ?? 0
})
const budgetDirty = computed(() => {
  const host = detailHost.value
  if (!host) return false
  return normalizedBudget() !== host.max_resource_slots
})
function normalizedBudget(): number {
  return Math.max(0, Math.trunc(Number(budgetDraft.value) || 0))
}

async function saveBudget() {
  const host = detailHost.value
  if (!host || savingBudget.value || !budgetDirty.value) return
  const next = normalizedBudget()
  if (next > 10_000) {
    await dialog.alert('Budget must be between 0 and 10000 (0 = inherit the platform default).')
    budgetDraft.value = host.max_resource_slots
    return
  }
  savingBudget.value = true
  try {
    await sandboxHostsApi.setBudget(host.host_id, host.instance_id, next)
    await load()
  } catch (e) {
    console.error(e)
    budgetDraft.value = host.max_resource_slots
    await dialog.alert('Failed to update the slot budget')
  } finally {
    savingBudget.value = false
  }
}

function cancelBudgetEdit() {
  budgetDraft.value = detailHost.value?.max_resource_slots ?? 0
}
const savingConfig = ref(false)
const configProblems = ref<string[]>([])
const configWarnings = ref<string[]>([])
const configValid = ref(false)
const versions = ref<ConfigVersion[]>([])
const versionsLoading = ref(false)

/** Version history pagination — cut client-side: the API already returns
 *  every version with its document (adjacent-version diffs depend on it),
 *  so slicing the in-memory list changes nothing about the diff data. */
const VERSIONS_PAGE_SIZE = 10
const versionPage = ref(1)
const versionPageCount = computed(() =>
  Math.max(1, Math.ceil(versions.value.length / VERSIONS_PAGE_SIZE)),
)
const pagedVersions = computed(() =>
  versions.value.slice(
    (versionPage.value - 1) * VERSIONS_PAGE_SIZE,
    versionPage.value * VERSIONS_PAGE_SIZE,
  ),
)

function gotoVersionPage(page: number) {
  versionPage.value = Math.min(Math.max(1, page), versionPageCount.value)
}

/** Structured form state — mirrors shared::sandbox_config::validate rules. */
type ProfileRow = { name: string; memory: string; cpu: string; pids: number; slots: number }
type TierRow = { name: string; image: string; runtime: string }
const formProfiles = ref<ProfileRow[]>([])
const formTiers = ref<TierRow[]>([])
const formNetwork = ref('')

const showRawJson = ref(false)
const rawJsonText = ref('')
/** Version history disclosure (host detail 方案 A): reference material, folded by default. */
const showVersionHistory = ref(false)
const resolvingTier = ref<string | null>(null)

const EMPTY_DOC: SandboxHostDocument = {
  profiles: {},
  tiers: {},
  network: '',
}

/** Placeholder shown in the Platform defaults dialog per tier. There is no
 *  official Buz job image — the operator points these at their own registry
 *  (e.g. the image their pipelines already use, with gVisor/kata runtimes
 *  added for the stronger levels). Empty = no default for that level. */
const TIER_IMAGE_HINTS: Record<string, string> = {
  container: 'registry.example.com/your-job-image:tag',
  sandbox: 'registry.example.com/your-job-image:gvisor',
  vm: 'registry.example.com/your-vm-image:tag',
}

/** A tier image is publishable once it is digest-pinned. */
function isPinnedImage(image: string): boolean {
  return image.includes('@sha256:')
}

/** Client-side mirror of the backend's validation (sandbox_config::validate). */
const PROFILE_NAME_RE = /^[a-z0-9][a-z0-9-]{0,31}$/
const MEMORY_RE = /^[0-9]+[bkmg]?$/
const TIER_NAMES = ['container', 'sandbox', 'vm'] as const

/** Same numbers as the catalog entry, string-compared the way the form holds
 *  them — a row that differs is a legacy host-side override. */
function sameAsCatalog(row: ProfileRow): boolean {
  const cat = platformDefaultProfiles.value[row.name]
  if (!cat) return false
  return (
    row.memory.trim() === String(cat.memory ?? '').trim() &&
    row.cpu.trim() === String(cat.cpu ?? '').trim() &&
    row.pids === (Number(cat?.pids) || 0) &&
    row.slots === (Number(cat?.slots) || 0)
  )
}

/** A tier row identical to the platform catalog means "inherited" — the same
 *  rule profiles follow. Publish strips catalog-identical rows, so their
 *  presence in a pasted document marks inherited rows, never host pins.
 *  A catalog tier with no image inherits nothing. */
function sameTierAsCatalog(row: { name: string; image: string; runtime: string }): boolean {
  const cat = platformDefaults.value[row.name]
  const img = String(cat?.image ?? '').trim()
  if (!cat || !img) return false
  return row.image.trim() === img && row.runtime.trim() === String(cat.runtime ?? '').trim()
}

/** Load the editor from a host's *effective* document (platform catalog
 *  merged under its own rows). The form keeps only what the host document
 *  itself must say: legacy override/host-only profile rows and the exclusion
 *  set — every other catalog entry is inherited implicitly. */
function loadFormFromDocument(doc: SandboxHostDocument) {
  formProfiles.value = Object.entries(doc.profiles ?? {})
    .map(([name, p]) => ({
      name,
      memory: String(p?.memory ?? ''),
      cpu: String(p?.cpu ?? ''),
      pids: Number(p?.pids) || 0,
      slots: Number(p?.slots) || 0,
    }))
    // Catalog-identical rows (default included) mean "inherited", never a
    // host pin: keeps the aliased default off the card wall.
    .filter((row) => !sameAsCatalog(row))
  excludedSizes.value = new Set(
    Object.keys(platformDefaultProfiles.value).filter(
      (name) => name !== 'default' && !(name in (doc.profiles ?? {})),
    ),
  )
  formTiers.value = Object.entries(doc.tiers ?? {})
    .map(([name, t]) => ({
      name,
      image: String(t?.image ?? ''),
      runtime: String(t?.runtime ?? ''),
    }))
    .filter((row) => !sameTierAsCatalog(row))
  const network = String(doc.network ?? '').trim()
  // A pin equal to the platform default is behaviorally identical to
  // inheriting it — keep the honest "inherit" form (empty) in the form.
  formNetwork.value = network && network === platformDefaultNetwork.value.trim() ? '' : network
}

// A validation verdict only describes the values it ran on.
watch([formProfiles, formTiers, formNetwork], () => {
  configValid.value = false
}, { deep: true })

// ── Job sizes: a selection surface over the platform catalog ──
/** Catalog profiles (non-default) as toggleable chips: a host offers every
 *  catalog entry unless it explicitly excludes it (exclude_profiles). */
/** "Smallest first" for every Job-sizes listing: slots weight ascending
 *  (what the host budget deducts per job), memory as tie-breaker, then name.
 *  Reorders automatically when an operator renumbers a profile. */
function memoryBytes(raw: string): number {
  const m = raw.trim().match(/^(\d+(?:\.\d+)?)\s*([kmgt]?)i?b?$/i)
  if (!m) return 0
  const mult = { k: 1024, m: 1024 ** 2, g: 1024 ** 3, t: 1024 ** 4 }[m[2].toLowerCase()] ?? 1
  return Number(m[1]) * mult
}

function sizeAscending(a: string, b: string): number {
  const pa = platformDefaultProfiles.value[a]
  const pb = platformDefaultProfiles.value[b]
  const bySlots = (Number(pa?.slots) || 1) - (Number(pb?.slots) || 1)
  if (bySlots) return bySlots
  const byMemory = memoryBytes(String(pa?.memory ?? '')) - memoryBytes(String(pb?.memory ?? ''))
  if (byMemory) return byMemory
  return a.localeCompare(b)
}

const catalogSizes = computed(() =>
  Object.keys(platformDefaultProfiles.value)
    .filter((name) => name !== 'default')
    .sort(sizeAscending),
)
/** The catalog profile whose numbers equal `default`'s (small preferred).
 *  `default` renders as a badge on it instead of a duplicate row/card; null
 *  once an operator diverges the numbers — then default shows standalone. */
const defaultAliasName = computed<string | null>(() => {
  const d = platformDefaultProfiles.value['default']
  if (!d) return null
  const same = (p: ResourceProfile | undefined) =>
    String(p?.memory ?? '').trim() === String(d.memory ?? '').trim() &&
    String(p?.cpu ?? '').trim() === String(d.cpu ?? '').trim() &&
    (Number(p?.pids) || 0) === (Number(d.pids) || 0) &&
    (Number(p?.slots) || 1) === (Number(d.slots) || 1)
  const matches = catalogSizes.value.filter((name) => same(platformDefaultProfiles.value[name]))
  if (!matches.length) return null
  return matches.includes('small') ? 'small' : matches[0]
})
/** Catalog profile names this host refuses — persisted as exclude_profiles. */
const excludedSizes = ref<Set<string>>(new Set())

function toggleCatalogSize(name: string) {
  const next = new Set(excludedSizes.value)
  if (next.has(name)) next.delete(name)
  else next.add(name)
  excludedSizes.value = next
}

/** The card wall shows only what this host offers; exclusions are the
 *  exception and live behind the manage panel instead of as cards. */
const offeredCatalogSizes = computed(() =>
  catalogSizes.value.filter((name) => !excludedSizes.value.has(name)),
)
const excludedCatalogSizes = computed(() =>
  catalogSizes.value.filter((name) => excludedSizes.value.has(name)),
)
/** Inline "manage catalog profiles" panel state (same pattern as the grants
 *  table's inline add-team panel). */
const showCatalogPanel = ref(false)

/** Legacy rows the host document still defines itself (host-only names, or
 *  numbers that differ from the catalog). Dropping one publishes without it;
 *  the UI no longer creates new ones — the catalog is edited in one place. */
function dropLegacyRow(row: ProfileRow) {
  const index = formProfiles.value.indexOf(row)
  if (index !== -1) formProfiles.value.splice(index, 1)
}

/** The fallback envelope: catalog numbers when known, else the row an
 *  existing host document still carries. */
const fallbackSize = computed<ProfileRow | null>(() => {
  const cat = platformDefaultProfiles.value['default']
  if (cat)
    return {
      name: 'default',
      memory: String(cat.memory ?? ''),
      cpu: String(cat.cpu ?? ''),
      pids: Number(cat.pids) || 0,
      slots: Number(cat.slots) || 1,
    }
  return formProfiles.value.find((p) => p.name === 'default') ?? null
})

/** Grant lookup: team names allowed to request each catalog profile. */
const grantsByProfile = computed(() => {
  const byProfile = new Map<string, string[]>()
  for (const g of grants.value) {
    const names = byProfile.get(g.profile) ?? []
    names.push(teamName(g.team_id))
    byProfile.set(g.profile, names)
  }
  return byProfile
})

function sizeGrantLabel(name: string): { text: string; granted: boolean } {
  const teams = grantsByProfile.value.get(name)
  if (!teams?.length) return { text: 'no grant', granted: false }
  return { text: teams.length === 1 ? teams[0] : `${teams[0]} +${teams.length - 1}`, granted: true }
}

function specLine(p: ProfileRow): string {
  return `${p.memory} · ${p.cpu} cpu · ${p.pids} pids · slot ${p.slots}`
}

function catalogSizeRow(name: string): ProfileRow {
  const p = platformDefaultProfiles.value[name]
  return {
    name,
    memory: p?.memory ?? '',
    cpu: p?.cpu ?? '',
    pids: Number(p?.pids) || 0,
    slots: Number(p?.slots) || 1,
  }
}

function openDetail(host: SandboxHost) {
  detailHost.value = host
  configProblems.value = []
  configWarnings.value = []
  configValid.value = false
  showRawJson.value = false
  showVersionHistory.value = false
  loadFormFromDocument(host.document ?? EMPTY_DOC)
  void loadVersions(host)
}



/** Validate the form rows against the backend rules; always returns a doc
 *  (best-effort) plus the problems that would make the server reject it.
 *  The published document is a pure selection: legacy profile rows the host
 *  still carries, the exclusion set, and the network pin (empty = inherit). */
function validateRows(): { doc: SandboxHostDocument; problems: string[] } {
  const problems: string[] = []
  const profiles: Record<string, ResourceProfile> = {}
  const seenProfiles = new Set<string>()
  for (const row of formProfiles.value) {
    const name = row.name.trim() || '(unnamed)'
    if (seenProfiles.has(name)) problems.push(`duplicate profile name '${name}'`)
    if (name !== 'default' && name !== '(unnamed)' && !PROFILE_NAME_RE.test(name)) {
      problems.push(`profile name '${name}' is invalid (expected [a-z0-9][a-z0-9-], max 32 chars)`)
    }
    if (!MEMORY_RE.test(row.memory.trim())) {
      problems.push(`profile '${name}': memory must be a size like 2g, 512m, or plain bytes`)
    }
    const cpu = Number(row.cpu)
    if (!row.cpu.trim() || !Number.isFinite(cpu) || cpu <= 0) {
      problems.push(`profile '${name}': cpu must be a positive number of cores (e.g. 2, 1.5)`)
    }
    if (!(row.pids >= 1)) problems.push(`profile '${name}': pids must be at least 1`)
    if (!(row.slots >= 1 && row.slots <= 32)) {
      problems.push(`profile '${name}': slots must be between 1 and 32`)
    }
    if (!seenProfiles.has(name)) {
      seenProfiles.add(name)
      profiles[name] = {
        memory: row.memory.trim(),
        cpu: row.cpu.trim(),
        pids: Math.trunc(row.pids) || 0,
        slots: Math.trunc(row.slots) || 0,
      }
    }
  }

  const tiers: Record<string, { image: string; runtime: string }> = {}
  const seenTiers = new Set<string>()
  for (const row of formTiers.value) {
    const name = row.name.trim() || '(unnamed)'
    if (seenTiers.has(name)) problems.push(`duplicate tier name '${name}'`)
    seenTiers.add(name)
    if (!(TIER_NAMES as readonly string[]).includes(name)) {
      problems.push(`tier '${name}' is unknown (expected container, sandbox, or vm)`)
    }
    // A tier with no own image inherits the platform default at apply time;
    // only flag it when no default exists either (publish would strand jobs).
    if (!row.image.trim()) {
      if (!platformDefaults.value[name]?.image) {
        problems.push(`tier '${name}': no image and no platform default — disable this tier or provide one`)
      }
      continue
    }
    if (!row.image.includes('@sha256:')) {
      problems.push(`tier '${name}': image must be a digest-pinned reference (repo@sha256:…)`)
    }
    if (!seenTiers.has(name)) {
      seenTiers.add(name)
      tiers[name] = { image: row.image.trim(), runtime: row.runtime.trim() }
    }
  }

  if (excludedSizes.value.has('default')) {
    problems.push("the 'default' profile cannot be excluded — it is the fallback envelope for jobs without runner.resources")
  }

  return {
    doc: {
      profiles,
      exclude_profiles: [...excludedSizes.value].filter((n) => n !== 'default').sort(),
      tiers,
      network: formNetwork.value.trim(),
    },
    problems,
  }
}

/** Loose dump for the raw-JSON escape hatch (no validation). Dumps the
 *  *effective* document — the host's own rows plus the catalog rows this
 *  host offers — mirroring what the list API serves and sandboxd fetches,
 *  so the raw view matches the form's 1:1 instead of showing only the
 *  host-owned subset. */
function dumpDocumentLoose(): SandboxHostDocument {
  const profiles: Record<string, ResourceProfile> = {}
  formProfiles.value.forEach((row, i) => {
    profiles[row.name.trim() || `unnamed-${i + 1}`] = {
      memory: row.memory,
      cpu: row.cpu,
      pids: row.pids,
      slots: row.slots,
    }
  })
  for (const name of offeredCatalogSizes.value) {
    if (name in profiles) continue
    const p = platformDefaultProfiles.value[name]
    profiles[name] = {
      memory: String(p?.memory ?? ''),
      cpu: String(p?.cpu ?? ''),
      pids: Number(p?.pids) || 0,
      slots: Number(p?.slots) || 1,
    }
  }
  if (!profiles.default) {
    const d = platformDefaultProfiles.value.default
    if (d) {
      profiles.default = {
        memory: String(d.memory ?? ''),
        cpu: String(d.cpu ?? ''),
        pids: Number(d.pids) || 0,
        slots: Number(d.slots) || 1,
      }
    }
  }
  const tiers: Record<string, { image: string; runtime: string }> = {}
  formTiers.value.forEach((row, i) => {
    tiers[row.name.trim() || `unnamed-${i + 1}`] = { image: row.image, runtime: row.runtime }
  })
  // Catalog tiers this host inherits fill the rest — the effective merge.
  for (const [name, t] of Object.entries(platformDefaults.value)) {
    if (!(name in tiers)) {
      tiers[name] = { image: String(t.image ?? ''), runtime: String(t.runtime ?? '') }
    }
  }
  return {
    profiles,
    exclude_profiles: [...excludedSizes.value].filter((n) => n !== 'default').sort(),
    tiers,
    network: formNetwork.value.trim() || platformDefaultNetwork.value.trim(),
  }
}

function toggleRawJson() {
  if (!showRawJson.value) {
    rawJsonText.value = JSON.stringify(dumpDocumentLoose(), null, 2)
    showRawJson.value = true
  } else if (applyRawToForm()) {
    showRawJson.value = false
  }
  // Invalid JSON: stay in the raw view — the shared problems list explains why.
}

/** Parse the textarea into the form. Returns false when the JSON is invalid;
 *  validation problems land in the shared red list either way. */
function applyRawToForm(): boolean {
  let parsed: unknown
  try {
    parsed = JSON.parse(rawJsonText.value)
  } catch (e) {
    configProblems.value = [`invalid JSON: ${e instanceof Error ? e.message : String(e)}`]
    return false
  }
  const obj = (parsed ?? {}) as Record<string, unknown>
  const profilesObj = (obj.profiles ?? {}) as Record<string, Record<string, unknown>>
  const tiersObj = (obj.tiers ?? {}) as Record<string, Record<string, unknown>>
  const rows = Object.entries(profilesObj).map(([name, p]) => ({
    name,
    memory: String(p?.memory ?? ''),
    cpu: String(p?.cpu ?? ''),
    pids: Number(p?.pids) || 0,
    slots: Number(p?.slots) || 0,
  }))
  // Format detection: a host-selection document never repeats catalog rows
  // verbatim (publish strips them), so their presence marks the *effective*
  // document — where a catalog name missing from `profiles` means excluded.
  // In either format, catalog-identical rows mean "inherited", never
  // host-pinned, and `default` always stays a host-visible row.
  const effectiveFormat = rows.some((row) => row.name !== 'default' && sameAsCatalog(row))
  formProfiles.value = rows.filter((row) => !sameAsCatalog(row))
  const present = new Set(Object.keys(profilesObj))
  const explicit = ((obj.exclude_profiles ?? []) as unknown[]).map(String)
  excludedSizes.value = new Set(
    (effectiveFormat
      ? [
          ...Object.keys(platformDefaultProfiles.value).filter(
            (name) => name !== 'default' && !present.has(name),
          ),
          ...explicit,
        ]
      : explicit
    ).filter((n) => n !== 'default'),
  )
  formTiers.value = Object.entries(tiersObj)
    .map(([name, t]) => ({
      name,
      image: String(t?.image ?? ''),
      runtime: String(t?.runtime ?? ''),
    }))
    .filter((row) => !sameTierAsCatalog(row))
  const pastedNetwork = String(obj.network ?? '').trim()
  formNetwork.value =
    pastedNetwork && pastedNetwork === platformDefaultNetwork.value.trim()
      ? ''
      : pastedNetwork
  configValid.value = false
  configProblems.value = validateRows().problems
  return true
}

/** Paste JSON → parse into the form without leaving the raw view. */
function applyRawJson() {
  applyRawToForm()
}

// ── platform default tiers + default profiles (inherited by hosts that leave
//    a tier or profile undefined) ──
const showDefaultsDialog = ref(false)
const defaultsRows = ref<TierRow[]>([])
const defaultsProblems = ref<string[]>([])
const savingDefaults = ref(false)
const platformDefaults = ref<Record<string, { image: string; runtime: string }>>({})
const platformDefaultProfiles = ref<Record<string, ResourceProfile>>({})
const platformDefaultNetwork = ref('bridge')
const platformDefaultSlots = ref(0)
const defaultProfileRows = ref<ProfileRow[]>([])

async function loadDefaults() {
  try {
    const d = await sandboxHostsApi.getDefaults()
    platformDefaults.value = d.tiers
    platformDefaultProfiles.value = d.profiles
    platformDefaultNetwork.value = d.network || 'bridge'
    platformDefaultSlots.value = d.max_slots || 0
  } catch {
    platformDefaults.value = {}
    platformDefaultProfiles.value = {}
    platformDefaultNetwork.value = 'bridge'
    platformDefaultSlots.value = 0
  }
}

function openDefaultsDialog() {
  // One row per known tier so the table reads the same every time.
  defaultsRows.value = TIER_NAMES.map((name) => {
    const existing = platformDefaults.value[name]
    return {
      name,
      image: existing?.image ?? '',
      runtime: existing?.runtime ?? '',
    }
  })
  // Profile rows: `default` is not listed — it is an implicit alias of
  // `small` and is written in lockstep on save.
  const catalog = { ...platformDefaultProfiles.value }
  delete catalog.default
  defaultProfileRows.value = Object.keys(catalog)
    .sort(sizeAscending)
    .map((name) => ({
      name,
      memory: catalog[name].memory,
      cpu: catalog[name].cpu,
      pids: Number(catalog[name].pids) || 0,
      slots: Number(catalog[name].slots) || 1,
    }))
  defaultsProblems.value = []
  showDefaultsDialog.value = true
}

function addDefaultProfileRow() {
  defaultProfileRows.value.push({ name: '', memory: '2g', cpu: '2', pids: 256, slots: 1 })
}

function removeDefaultProfileRow(index: number) {
  const row = defaultProfileRows.value[index]
  if (row?.name === 'small') return
  defaultProfileRows.value.splice(index, 1)
}

async function resolveDefaultsRow(row: TierRow) {
  const raw = row.image.trim()
  if (!raw || isPinnedImage(raw)) return
  resolvingTier.value = raw
  defaultsProblems.value = []
  try {
    row.image = await sandboxHostsApi.resolveImage(raw)
  } catch (e) {
    defaultsProblems.value = [`tier '${row.name}': ${e instanceof Error ? e.message : String(e)}`]
  } finally {
    resolvingTier.value = null
  }
}

async function saveDefaults() {
  defaultsProblems.value = []
  const tiers: Record<string, { image: string; runtime: string }> = {}
  for (const row of defaultsRows.value) {
    const image = row.image.trim()
    if (!image) continue
    if (!isPinnedImage(image)) {
      resolvingTier.value = row.name
      try {
        row.image = await sandboxHostsApi.resolveImage(image)
      } catch (e) {
        resolvingTier.value = null
        defaultsProblems.value = [
          `tier '${row.name}': ${e instanceof Error ? e.message : String(e)}`,
        ]
        return
      }
      resolvingTier.value = null
    }
    tiers[row.name] = { image: row.image.trim(), runtime: row.runtime.trim() }
  }
  // Profile catalog: name + numeric fields; 'default' is required.
  const profiles: Record<string, ResourceProfile> = {}
  let profileProblems: string[] = []
  const seenNames = new Set<string>()
  for (const row of defaultProfileRows.value) {
    const name = row.name.trim()
    if (!name) continue
    if (seenNames.has(name)) {
      profileProblems.push(`duplicate profile name '${name}'`)
      continue
    }
    seenNames.add(name)
    if (!PROFILE_NAME_RE.test(name)) {
      profileProblems.push(`profile '${name}' name must match [a-z0-9][a-z0-9-]{0,31}`)
    }
    if (!MEMORY_RE.test(row.memory.trim())) {
      profileProblems.push(`profile '${name}': memory must be a size like 2g, 512m`)
    }
    const cpu = Number(row.cpu)
    if (!Number.isFinite(cpu) || cpu <= 0) {
      profileProblems.push(`profile '${name}': cpu must be a positive number`)
    }
    if (!(row.pids >= 1)) profileProblems.push(`profile '${name}': pids must be at least 1`)
    if (!(row.slots >= 1 && row.slots <= 32)) {
      profileProblems.push(`profile '${name}': slots must be between 1 and 32`)
    }
    profiles[name] = {
      memory: row.memory.trim(),
      cpu: row.cpu.trim(),
      pids: Math.trunc(row.pids) || 0,
      slots: Math.trunc(row.slots) || 0,
    }
  }
  // 'default' is an implicit alias of 'small': the row is not listed but the
  // backend requires the entry — save writes it with small's numbers so the
  // fallback envelope and the small size stay in lockstep.
  const smallRow = defaultProfileRows.value.find((row) => row.name.trim() === 'small')
  if (smallRow) {
    profiles['default'] = {
      memory: smallRow.memory.trim(),
      cpu: smallRow.cpu.trim(),
      pids: Math.trunc(smallRow.pids) || 0,
      slots: Math.trunc(smallRow.slots) || 0,
    }
  } else {
    profileProblems.unshift("profiles must include the mandatory 'small' job size")
  }
  if (profileProblems.length) {
    defaultsProblems.value = [...defaultsProblems.value, ...profileProblems]
    return
  }

  savingDefaults.value = true
  try {
    const result = await sandboxHostsApi.putDefaults({
      tiers,
      profiles,
      network: platformDefaultNetwork.value.trim() || 'bridge',
      max_slots: platformDefaultSlots.value,
    })
    platformDefaults.value = tiers
    platformDefaultProfiles.value = profiles
    showDefaultsDialog.value = false
    await load()
    await dialog.alert(
      result.bumped_hosts.length
        ? `Defaults saved — ${result.bumped_hosts.length} host(s) got a new desired version and will re-fetch: ${result.bumped_hosts.join(', ')}`
        : 'Defaults saved — no host needed a new version (each one defines its own tiers/sizes).',
    )
  } catch (e) {
    defaultsProblems.value = [e instanceof Error ? e.message : String(e)]
  } finally {
    savingDefaults.value = false
  }
}

/** Tier names a host offers, from its effective document (platform defaults merged). */
function offeredTierNames(host: SandboxHost): string[] {
  return Object.keys(host.document?.tiers ?? {})
}

async function load() {
  loading.value = true
  try {
    const [hostList, changeList, teamList, grantList] = await Promise.all([
      sandboxHostsApi.list(),
      sandboxHostsApi.changes('pending').then((r) => r.changes),
      teamsApi.list().catch(() => [] as Team[]),
      sandboxHostsApi.grants().catch(() => [] as ResourceGrant[]),
    ])
    hosts.value = hostList
    changes.value = changeList
    teams.value = teamList
    grants.value = grantList
    await loadDefaults()
    if (detailHost.value) {
      detailHost.value =
        hostList.find(
          (h) =>
            h.host_id === detailHost.value!.host_id &&
            h.instance_id === detailHost.value!.instance_id,
        ) ?? null
    }
  } finally {
    loading.value = false
  }
}

onMounted(load)

// Deep link from the top-nav bell: land on the approvals queue.
if (route.query.approvals === '1') approvalsTab.value = 'pending'

function openEnroll() {
  enrollHostId.value = ''
  enrollInstanceId.value = ''
  enrollSlots.value = 0
  issuedToken.value = ''
  tokenCopied.value = false
  snippetCopied.value = false
  enrollError.value = ''
  showAdvanced.value = false
  instanceEdited.value = false
  showEnroll.value = true
}

async function handleEnroll() {
  const hostId = enrollHostId.value.trim()
  const instanceId = enrollInstanceId.value.trim()
  if (!hostId || !instanceId) return
  for (const [what, value] of [['Host ID', hostId], ['Instance ID', instanceId]] as const) {
    if (!ENROLL_ID_RE.test(value)) {
      enrollError.value = `${what}: 1–64 characters, starting with a letter or digit; only letters, digits, '.', '_' and '-' are allowed.`
      return
    }
  }
  enrollError.value = ''
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

async function copySnippet() {
  const { copyText } = await import('@/utils/copyText')
  await copyText(bootstrapSnippet.value)
  snippetCopied.value = true
  setTimeout(() => {
    snippetCopied.value = false
  }, 1500)
}

async function removeHost(host: SandboxHost) {
  const ok = await dialog.confirm(
    `Delete enrollment ${host.host_id}/${host.instance_id}? Its config versions, pending changes and team bindings are removed.`,
  )
  if (!ok) return
  try {
    await sandboxHostsApi.remove(host.host_id, host.instance_id)
    if (
      detailHost.value?.host_id === host.host_id &&
      detailHost.value?.instance_id === host.instance_id
    ) {
      detailHost.value = null
    }
    await load()
  } catch (e) {
    console.error(e)
    await dialog.alert('Delete failed — the Manager refuses while runners are still bound to this host')
    await load()
  }
}

async function loadVersions(host: SandboxHost) {
  versionsLoading.value = true
  versionPage.value = 1
  try {
    versions.value = await sandboxHostsApi.versions(host.host_id, host.instance_id)
  } catch {
    versions.value = []
  } finally {
    versionsLoading.value = false
  }
}

/** Validate + publish both go through here: tag-form tier images are resolved
 *  to digest-pinned references first (people pick the tag, the Manager pins
 *  the digest), then the rows are checked against the backend rules. */
async function prepareDocument(): Promise<SandboxHostDocument | null> {
  configProblems.value = []
  for (const row of formTiers.value) {
    const raw = row.image.trim()
    if (!raw || isPinnedImage(raw)) continue
    resolvingTier.value = raw
    try {
      row.image = await sandboxHostsApi.resolveImage(raw)
    } catch (e) {
      resolvingTier.value = null
      configProblems.value = [
        `tier '${row.name}': ${e instanceof Error ? e.message : String(e)}`,
      ]
      return null
    }
    resolvingTier.value = null
  }
  const { doc, problems } = validateRows()
  if (problems.length) {
    configProblems.value = problems
    return null
  }
  return doc
}

async function validateConfig() {
  if (!detailHost.value) return
  configValid.value = false
  const doc = await prepareDocument()
  if (!doc) return
  const result = await sandboxHostsApi.validate(detailHost.value.host_id, detailHost.value.instance_id, doc)
  configProblems.value = result.problems ?? []
  configWarnings.value = result.warnings ?? []
  configValid.value = !configProblems.value.length && !configWarnings.value.length
}

async function saveConfig() {
  if (!detailHost.value) return
  const doc = await prepareDocument()
  if (!doc) return
  savingConfig.value = true
  try {
    const result = await sandboxHostsApi.putConfig(
      detailHost.value.host_id,
      detailHost.value.instance_id,
      doc,
      detailHost.value.desired_version,
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
    if (detailHost.value?.document) loadFormFromDocument(detailHost.value.document)
    if (detailHost.value) await loadVersions(detailHost.value)
  } catch (e) {
    console.error(e)
    await dialog.alert('Failed to publish configuration (stale If-Match?) — reload and retry')
    await load()
    if (detailHost.value?.document) loadFormFromDocument(detailHost.value.document)
  } finally {
    savingConfig.value = false
  }
}

/** One-line change description for a stored config version, diffed against
 *  the previous version's document (the history table keeps no description
 *  column, and the versions API returns every document). Describing the raw
 *  selection doc instead read as "no tiers · profiles: default" — misleading,
 *  since catalog-inherited rows never appear in it. The first version (or an
 *  adjacency gap) describes the initial publication. */
function versionEvent(v: ConfigVersion, prev?: ConfigVersion): string {
  const cur = v.document
  if (!cur || typeof cur !== 'object') return 'empty document'
  const hasPrev = !!prev && prev.version === v.version - 1 && !!prev.document
  const before = (hasPrev ? prev!.document : {}) as Partial<SandboxHostDocument>
  const parts: string[] = []

  // Exclusions — what this host stopped / started offering from the catalog.
  const curEx = new Set((cur.exclude_profiles ?? []).filter((n) => n !== 'default'))
  const prevEx = new Set((before.exclude_profiles ?? []).filter((n) => n !== 'default'))
  for (const name of [...curEx].filter((n) => !prevEx.has(n)).sort()) {
    parts.push(hasPrev ? `stop offering '${name}'` : `excludes '${name}'`)
  }
  for (const name of [...prevEx].filter((n) => !curEx.has(n)).sort()) {
    parts.push(`offer '${name}' again`)
  }

  // Host-defined profile rows (overrides over the catalog).
  const curP = cur.profiles ?? {}
  const prevP = before.profiles ?? {}
  for (const name of Object.keys(curP).sort()) {
    const b = curP[name]
    if (!prevP[name]) parts.push(`host override '${name}': ${b.memory} · ${b.cpu} cpu`)
    else if (JSON.stringify(prevP[name]) !== JSON.stringify(b)) {
      parts.push(`host override '${name}' updated`)
    }
  }
  for (const name of Object.keys(prevP).sort()) {
    if (!curP[name]) parts.push(`host override '${name}' dropped — inherits catalog`)
  }

  // Tier pins.
  const curT = cur.tiers ?? {}
  const prevT = before.tiers ?? {}
  for (const name of Object.keys(curT).sort()) {
    if (!prevT[name]) parts.push(`tier pin '${name}' added`)
    else if (JSON.stringify(prevT[name]) !== JSON.stringify(curT[name])) {
      parts.push(`tier pin '${name}' updated`)
    }
  }
  for (const name of Object.keys(prevT).sort()) {
    if (!curT[name]) parts.push(`tier pin '${name}' dropped — inherits catalog`)
  }

  // Network pin.
  const curNet = (cur.network ?? '').trim()
  const prevNet = (before.network ?? '').trim()
  if (curNet !== prevNet) {
    parts.push(curNet ? `network pinned to '${curNet}'` : 'network set to inherit')
  }

  if (!parts.length) {
    return hasPrev ? 'no content change — republished' : 'initial publication — catalog defaults only'
  }
  return parts.length > 4
    ? `${parts.slice(0, 4).join(' · ')} · +${parts.length - 4} more`
    : parts.join(' · ')
}

async function rollbackTo(version: number) {
  if (!detailHost.value) return
  const ok = await dialog.confirm(`Restore version ${version} as a new version?`)
  if (!ok) return
  try {
    const result = await sandboxHostsApi.rollback(detailHost.value.host_id, detailHost.value.instance_id, version)
    await dialog.alert(`Rolled back — published as version ${result.version}`)
    await load()
    if (detailHost.value?.document) loadFormFromDocument(detailHost.value.document)
    if (detailHost.value) await loadVersions(detailHost.value)
  } catch (e) {
    console.error(e)
    await dialog.alert('Rollback failed')
  }
}

async function decide(change: PendingChange, approve: boolean) {
  try {
    await sandboxHostsApi.decideChange(change.id, approve)
    await load()
  } catch (e) {
    console.error(e)
    await dialog.alert(approve ? 'Approval failed (self-approval?)' : 'Rejection failed')
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
</script>

<template>
  <div class="space-y-4">
    <div class="flex items-center justify-between gap-3 flex-wrap">
      <p class="type-meta max-w-3xl">
        Enrolled buz-sandboxd instances, one per <code class="mono-chip text-[10px]">host_id/instance_id</code>. Click a host to manage its config document (which resource profiles it offers) and its per-host access whitelist. Enrolling issues the one-time token sandboxd needs to fetch its document from this Manager.
      </p>
      <div class="flex items-center gap-2">
        <UiButton size="sm" variant="ghost" :loading="loading" @click="load">
          <RefreshCw class="h-4 w-4" />
        </UiButton>
        <UiButton size="sm" variant="outline" @click="openDefaultsDialog">Platform defaults</UiButton>
      </div>
    </div>

    <!-- ── Fleet policy: profile grants (one line per team) ── -->
    <div>
      <div class="flex items-end justify-between gap-3">
        <div>
          <h2 class="text-[13px] font-semibold tracking-tight">Fleet policy · Profile grants</h2>
          <p class="mt-0.5 text-[10.5px] text-muted-foreground">
            Which teams may request each non-default profile, fleet-wide — <code class="mono">default</code> is implicit for every team.
          </p>
        </div>
        <span class="pb-0.5 text-[10px] tabular-nums text-muted-foreground">{{ grants.length }} grants · {{ grantTeamRows.length }} teams</span>
      </div>
      <div class="mt-2 rounded-xl border border-border/50 bg-card overflow-hidden">
        <div class="data-table-scroll">
          <div class="min-w-[560px]">
            <div class="grid grid-cols-[190px_minmax(0,1fr)_120px] gap-2 border-b border-border/40 px-5 py-2 text-[9.5px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">
              <span>Team</span>
              <span>Granted profiles</span>
              <span></span>
            </div>
            <div class="divide-y divide-border/30">
              <div v-for="team in grantTeamRows" :key="team.id" class="px-5 py-2">
                <div class="grid grid-cols-[190px_minmax(0,1fr)_120px] items-center gap-2">
                  <div class="flex min-w-0 items-center gap-2">
                    <span
                      class="flex h-5 w-5 shrink-0 items-center justify-center rounded-full text-[10px] font-medium"
                      :style="{ background: teamTint(team.id) + '22', color: teamTint(team.id) }"
                      aria-hidden="true"
                    >{{ team.name.charAt(0).toUpperCase() }}</span>
                    <span class="truncate text-[12.5px] font-medium">{{ team.name }}</span>
                  </div>
                  <div
                    class="flex min-w-0 items-center gap-1.5"
                    :class="expandedGrantTeams.has(team.id) ? 'flex-wrap' : 'flex-nowrap overflow-hidden'"
                  >
                    <span
                      v-for="g in visibleGrants(team.id)"
                      :key="g.team_id + '/' + g.profile"
                      class="inline-flex shrink-0 items-center gap-1 rounded-full bg-muted py-0.5 pl-2.5 pr-1 font-mono text-[11px] text-foreground/80"
                      :title="offeredByFor(g.profile).length
                        ? `offered by ${offeredByFor(g.profile).join(', ')}`
                        : 'not offered on any host yet'"
                    >
                      {{ g.profile }}
                      <button
                        type="button"
                        class="inline-flex h-[18px] w-[18px] items-center justify-center rounded-full bg-foreground/15 text-foreground/70 transition-colors hover:bg-red-500/20 hover:text-red-500"
                        :title="`Revoke ${g.profile} from ${team.name} — stops requests at trigger & claim`"
                        :aria-label="`Revoke ${g.profile} from ${team.name}`"
                        @click="revokeGrant(g)"
                      ><X class="h-3 w-3" /></button>
                    </span>
                    <button
                      v-if="teamGrants(team.id).length > CHIP_PREVIEW"
                      type="button"
                      class="shrink-0 rounded-full border border-dashed border-border px-2.5 py-0.5 text-[10.5px] text-muted-foreground transition-colors hover:border-muted-foreground/60 hover:text-foreground"
                      :title="expandedGrantTeams.has(team.id) ? 'Collapse' : 'Show all profiles'"
                      @click="toggleGrantsExpanded(team.id)"
                    >{{ expandedGrantTeams.has(team.id) ? 'less' : `+${teamGrants(team.id).length - CHIP_PREVIEW} more` }}</button>
                  </div>
                  <div class="flex justify-end gap-1">
                    <UiButton
                      size="sm"
                      variant="ghost"
                      @click="grantPanelTeamId === team.id ? closeGrantPanel() : openGrantPanel(team.id)"
                    >
                      <Plus class="h-3.5 w-3.5" /> {{ grantPanelTeamId === team.id ? 'Close' : 'Grant' }}
                    </UiButton>
                    <UiButton
                      size="sm"
                      variant="ghost"
                      class="-mr-2"
                      :loading="revokingTeamId === team.id"
                      :title="`Remove all ${teamGrants(team.id).length} grants from ${team.name}`"
                      :aria-label="`Remove all grants from ${team.name}`"
                      @click="revokeTeamGrants(team)"
                    ><Trash2 class="h-3.5 w-3.5" /></UiButton>
                  </div>
                </div>

                <!-- inline multi-grant panel: search + multi-select, one action grants all -->
                <div v-if="grantPanelTeamId === team.id" class="mt-2 rounded-lg border border-border/60 bg-muted/25 p-2">
                  <div class="flex flex-wrap items-center gap-2 px-1 pb-2">
                    <div class="relative max-w-[240px] flex-1">
                      <Search class="pointer-events-none absolute left-2 top-1/2 h-3 w-3 -translate-y-1/2 text-muted-foreground" />
                      <input
                        v-model="grantPanelSearch"
                        type="search"
                        placeholder="Search profiles…"
                        aria-label="Search profiles"
                        class="w-full rounded-md border border-border/60 bg-background py-1 pl-7 pr-2 text-[11px] outline-hidden focus:border-primary/50"
                      />
                    </div>
                    <span class="text-[10px] text-muted-foreground">pick any number — one action grants all</span>
                  </div>
                  <div v-if="grantPanelChoices.length" class="max-h-52 space-y-px overflow-y-auto">
                    <label
                      v-for="name in grantPanelChoices"
                      :key="name"
                      class="flex cursor-pointer items-center gap-2 rounded-md px-1.5 py-1.5 transition-colors hover:bg-muted/50"
                      :class="!offeredByFor(name).length && 'opacity-55'"
                    >
                      <input
                        type="checkbox"
                        class="h-3.5 w-3.5 shrink-0 accent-primary"
                        :checked="grantPanelPending.has(name) || isTeamGranted(team.id, name)"
                        :disabled="isTeamGranted(team.id, name)"
                        :aria-label="`Grant ${name} to ${team.name}`"
                        @change="togglePanelPending(name)"
                      />
                      <span class="min-w-0 flex-1 truncate font-mono text-[11.5px] font-medium">{{ name }}</span>
                      <span
                        v-if="isTeamGranted(team.id, name)"
                        class="shrink-0 rounded bg-muted px-1 py-px text-[9px] text-muted-foreground"
                      >granted</span>
                      <span class="flex shrink-0 flex-wrap gap-0.5">
                        <span
                          v-for="h in offeredByFor(name)"
                          :key="h"
                          class="rounded bg-emerald-500/10 px-1 py-px font-mono text-[9.5px] leading-snug text-emerald-700 dark:text-emerald-400"
                        >{{ h }}</span>
                      </span>
                    </label>
                  </div>
                  <p v-else class="px-1.5 py-2 text-[11px] italic text-muted-foreground/70">no profiles match “{{ grantPanelSearch }}”</p>
                  <div class="flex items-center gap-2 px-1 pt-2">
                    <UiButton
                      size="sm"
                      :loading="grantSaving"
                      :disabled="!grantPanelPending.size"
                      @click="applyPanelGrants"
                    >Grant{{ grantPanelPending.size ? ` (${grantPanelPending.size})` : '' }}</UiButton>
                    <p v-if="grantError" class="text-[10px] text-destructive">{{ grantError }}</p>
                  </div>
                </div>
              </div>

              <!-- new binding: pick an unbound team; its row opens with the grant panel -->
              <div class="flex items-center gap-2 px-5 py-2">
                <span class="text-[11px] text-muted-foreground">Grant to team</span>
                <select
                  v-model="newBindingTeamId"
                  class="rounded-md border border-border/60 bg-background px-2 py-1 text-[11.5px] outline-hidden focus:border-primary/50"
                  aria-label="Pick a team to grant profiles"
                  @change="pickNewBindingTeam"
                >
                  <option value="" disabled>Pick a team…</option>
                  <option v-for="t in unboundTeams" :key="t.id" :value="t.id">{{ t.name }}</option>
                </select>
                <span v-if="!unboundTeams.length" class="text-[10.5px] text-muted-foreground/60">every team already holds bindings</span>
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- ── Machines: enrolled sandboxd instances ── -->
        <!-- Grants/host approval inbox: Pending + History tabs -->
    <div v-if="changes.length || historyRows.length" class="rounded-xl border border-amber-500/40 bg-amber-500/5 p-4 space-y-2">
      <div class="flex items-center justify-between gap-2">
        <h3 class="text-sm font-semibold tracking-tight">Grant approvals</h3>
        <div class="inline-flex rounded-lg bg-muted p-0.5">
          <button
            type="button"
            class="rounded-md px-3 py-1 text-[11.5px] font-medium"
            :class="approvalsTab === 'pending' ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground'"
            @click="approvalsTab = 'pending'"
          >Pending <span class="mono text-[10px]">{{ changes.length }}</span></button>
          <button
            type="button"
            class="rounded-md px-3 py-1 text-[11.5px] font-medium"
            :class="approvalsTab === 'history' ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground'"
            @click="switchHistory"
          >History <span class="mono text-[10px]">{{ historyTotal }}</span></button>
        </div>
      </div>

      <div class="space-y-0.5">
        <p v-if="approvalsTab === 'pending' && !changes.length" class="rounded-lg px-2 py-3 text-[11.5px] text-muted-foreground">Queue is clear — new grant/revoke requests will appear here for approval</p>
        <p v-if="approvalsTab === 'history' && !historyRows.length" class="rounded-lg px-2 py-3 text-[11.5px] text-muted-foreground">No decided records yet</p>

        <div
          v-for="(change, i) in approvalsTab === 'pending' ? changes : []"
          :key="'p-' + change.id"
          class="flex items-center gap-2.5 rounded-lg px-2.5 py-1.5 hover:bg-muted/60 text-[11.5px]"
        >
          <span
            class="flex h-[18px] w-[18px] shrink-0 items-center justify-center rounded-full font-mono text-[10px] font-medium"
            :class="change.instance_id === 'grant' ? 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-400' : 'bg-red-500/12 text-red-700 dark:text-red-400'"
          >{{ i + 1 }}</span>
          <div class="min-w-0 flex-1">
            <span v-if="change.host_id === 'resource-grants'" class="inline-flex items-center gap-1.5 flex-wrap">
              <span class="font-medium" :class="changeGrantInfo(change).granted ? 'text-emerald-700 dark:text-emerald-400' : 'text-red-700 dark:text-red-400'">{{ change.instance_id === 'grant' ? 'Grant' : 'Revoke' }}</span>
              <span class="mono">{{ changeGrantInfo(change).profile }}</span>
              <span class="text-muted-foreground">{{ change.instance_id === 'grant' ? '→' : 'from' }}</span>
              <span class="font-medium">{{ changeGrantInfo(change).team_id }}</span>
              <span class="text-muted-foreground">· by {{ change.requested_by }} · {{ formatDateTime(change.created_at) }}</span>
              <span v-if="change.instance_id === 'revoke' && changeGrantInfo(change).queued_jobs" class="text-red-600 dark:text-red-400">· {{ changeGrantInfo(change).queued_jobs }} queued job(s) would fail at claim</span>
              <span v-if="change.instance_id === 'grant' && changeGrantInfo(change).pipelines != null" class="text-amber-600">· unlocks {{ changeGrantInfo(change).pipelines }} pipeline(s)</span>
            </span>
            <span v-else class="inline-flex items-center gap-1.5">
              <span class="mono">host config</span>
              <span class="text-muted-foreground">{{ change.host_id }}/{{ change.instance_id }}</span>
              <span class="text-muted-foreground">· by {{ change.requested_by }} · {{ formatDateTime(change.created_at) }}</span>
            </span>
          </div>
          <div class="flex shrink-0 gap-1.5">
            <UiButton size="sm" @click="decide(change, true)"><Check class="h-3 w-3" /> Approve</UiButton>
            <UiButton size="sm" variant="ghost" @click="decide(change, false)"><X class="h-3 w-3" /> Reject</UiButton>
          </div>
        </div>

        <div
          v-for="h in approvalsTab === 'history' ? historyRows : []"
          :key="'h-' + h.id"
          class="flex items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-[11.5px]"
        >
          <span class="flex h-[18px] w-[18px] shrink-0 items-center justify-center rounded-full bg-muted font-mono text-[10px] text-muted-foreground">·</span>
          <div class="min-w-0 flex-1">
            <span v-if="h.host_id === 'resource-grants'" class="inline-flex items-center gap-1.5 flex-wrap">
              <span class="font-medium" :class="changeGrantInfo(h).granted ? 'text-emerald-700 dark:text-emerald-400' : 'text-red-700 dark:text-red-400'">{{ h.instance_id === 'grant' ? 'Grant' : 'Revoke' }}</span>
              <span class="mono">{{ changeGrantInfo(h).profile }}</span>
              <span class="text-muted-foreground">{{ h.instance_id === 'grant' ? '→' : 'from' }}</span>
              <span class="font-medium">{{ changeGrantInfo(h).team_id }}</span>
            </span>
            <span v-else class="inline-flex items-center gap-1.5">
              <span class="mono">host config</span>
              <span class="text-muted-foreground">{{ h.host_id }}/{{ h.instance_id }}</span>
            </span>
            <span v-if="h.decision_comment" class="text-muted-foreground">“{{ h.decision_comment }}”</span>
          </div>
          <span class="inline-flex shrink-0 items-center rounded px-1.5 text-[10px] font-medium uppercase tracking-wide" :class="h.status === 'approved' ? 'bg-emerald-500/12 text-emerald-600 dark:text-emerald-400' : 'bg-red-500/12 text-red-600 dark:text-red-400'">{{ h.status }}</span>
          <span class="shrink-0 text-muted-foreground">by {{ h.requested_by }} · decided by {{ h.approved_by || '—' }} · {{ formatDateTime(h.decided_at || h.created_at) }}</span>
        </div>

        <div v-if="approvalsTab === 'history' && historyTotalPages > 1" class="flex items-center justify-end gap-2 pt-1.5 text-[11px] text-muted-foreground">
          <UiButton
            size="sm"
            variant="ghost"
            :disabled="historyPage <= 1 || historyLoading"
            @click="loadHistory(historyPage - 1)"
          >‹ Prev</UiButton>
          <span class="tabular-nums">{{ historyPage }} / {{ historyTotalPages }}</span>
          <UiButton
            size="sm"
            variant="ghost"
            :disabled="historyPage >= historyTotalPages || historyLoading"
            @click="loadHistory(historyPage + 1)"
          >Next ›</UiButton>
        </div>
      </div>
    </div>

    <div class="mt-6">
      <div class="min-w-0 space-y-4">
        <div>
          <div class="flex items-center gap-3">
            <h2 class="text-sm font-semibold tracking-tight">Enrolled hosts</h2>
            <span class="h-px flex-1 bg-border/50"></span>
            <UiButton size="sm" @click="openEnroll"><Plus class="h-4 w-4" /> Enroll host</UiButton>
          </div>
          <p class="mt-0.5 text-[10.5px] text-muted-foreground">buz-sandboxd instances by host — each offers the platform catalog unless overridden.</p>
        </div>
        <div v-if="loading" class="surface-card p-8 text-center type-meta">Loading…</div>
        <div v-else-if="hosts.length === 0" class="rounded-xl border border-border/50 bg-card p-16 text-center text-muted-foreground">
          <Boxes class="h-10 w-10 mx-auto mb-3 text-muted-foreground/20" />
          <p class="text-sm">No sandbox hosts enrolled</p>
          <p class="text-xs mt-1">Enroll a (host_id, instance_id) pair, then point buz-sandboxd's BUZ_MANAGER_URL + token file at this Manager.</p>
          <UiButton size="sm" class="mt-4" @click="openEnroll"><Plus class="h-4 w-4" /> Enroll host</UiButton>
        </div>
        <div v-else class="grid gap-4 lg:grid-cols-2">
          <div
            v-for="host in hosts"
            :key="host.host_id + '/' + host.instance_id"
            class="flex cursor-pointer flex-col gap-2 rounded-xl border bg-card p-4 hover:border-primary/40"
            :class="detailHost?.instance_id === host.instance_id && detailHost?.host_id === host.host_id ? 'border-primary/60' : 'border-border/50'"
            @click="openDetail(host)"
          >
            <div class="flex items-center justify-between gap-2">
              <p class="truncate text-sm font-medium">{{ host.host_id }} / {{ host.instance_id }}</p>
              <div class="flex shrink-0 items-center gap-2">
                <span v-if="host.pushed" class="text-[11px] tabular-nums text-muted-foreground">v{{ host.applied_version }}<span v-if="host.applied_at"> · {{ formatDateTime(host.applied_at) }}</span></span>
                <span v-else-if="host.desired_version > 0" class="text-[11px] font-medium tabular-nums text-amber-600">v{{ host.applied_version }} → v{{ host.desired_version }}</span>
                <span
                  class="inline-flex shrink-0 items-center gap-1.5 rounded-full border border-border/50 px-2 py-0.5 text-[11px] font-medium"
                  :class="host.live ? 'text-emerald-600 dark:text-emerald-400' : 'text-muted-foreground'"
                >
                  <span class="h-1.5 w-1.5 rounded-full" :class="host.live ? 'bg-emerald-500' : 'bg-muted-foreground/40'" />
                  {{ host.live ? 'online' : 'offline' }}
                </span>
              </div>
            </div>
            <p class="text-xs text-muted-foreground">
              <span v-if="host.pushed" class="font-medium text-emerald-600 dark:text-emerald-400">synced</span>
              <span v-else-if="host.desired_version > 0" class="font-medium text-amber-600">drift</span>
              <span v-else class="font-medium">no config</span>
              <span> · budget {{ host.max_resource_slots || '∞' }} slots<template v-if="!host.max_resource_slots"> <span class="text-muted-foreground/60">(platform default)</span></template></span>
            </p>
            <div class="flex flex-wrap items-baseline gap-1.5">
              <span class="mr-0.5 text-[11px] uppercase tracking-wide text-muted-foreground">offers</span>
              <span
                v-for="t in offeredTierNames(host)"
                :key="'tier-' + t"
                class="rounded-md bg-muted px-2 py-0.5 text-[11px] font-medium"
                :class="host.desired_version > 0 ? '' : 'opacity-55'"
              >{{ t }}</span>
              <span
                v-for="p in offeredProfileNames(host)"
                :key="'size-' + p"
                class="rounded-md bg-muted px-2 py-0.5 text-[11px] font-medium"
                :class="host.desired_version > 0 ? '' : 'opacity-55'"
              >{{ p }}</span>
            </div>
            <p class="text-xs text-muted-foreground">
              runners {{ host.attached_runners?.length || 0 }}<span v-if="host.attached_runners?.length"> · {{ host.attached_runners.join(', ') }}</span>
            </p>
            <div v-if="host.last_fetch_error" class="flex items-start gap-1.5 text-xs text-destructive">
              <AlertTriangle class="mt-0.5 h-3 w-3 shrink-0" />
              <span class="min-w-0 break-words">{{ host.last_fetch_error }}</span>
            </div>
            <div class="mt-auto flex items-center gap-2 pt-1" @click.stop>
              <UiButton size="sm" variant="ghost" @click="rotateToken(host)"><KeyRound class="h-3.5 w-3.5" /> Rotate token</UiButton>
              <UiButton
                size="sm"
                variant="ghost"
                class="ml-auto"
                :disabled="!!host.attached_runners?.length"
                :title="host.attached_runners?.length
                  ? `${host.attached_runners.length} runner(s) still bound — detach them before deleting`
                  : 'Delete enrollment'"
                @click="removeHost(host)"
              ><Trash2 class="h-3.5 w-3.5" /></UiButton>
            </div>
          </div>
        </div>
      </div>

      <p v-if="grantsPendingHint" class="rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-[11px] text-amber-700 dark:text-amber-400">{{ grantsPendingHint }}</p>

    <!-- ── host detail dialog: desired configuration ── -->
    <div v-if="detailHost" class="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4" @click.self="detailHost = null">
      <div class="flex max-h-[85vh] w-full max-w-3xl flex-col overflow-hidden rounded-xl border border-border/60 bg-card shadow-lg">
        <div class="shrink-0 border-b border-border/50 px-6 py-4">
          <div class="flex items-start justify-between gap-3">
            <h3 class="min-w-0 truncate text-base font-semibold tracking-tight">
              {{ detailHost.host_id }} <span class="font-normal text-muted-foreground">/ {{ detailHost.instance_id }}</span>
            </h3>
            <div class="flex items-center gap-2 shrink-0">
              <span
                class="inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium uppercase tracking-wide"
                :class="detailHost.pushed ? 'bg-emerald-500/12 text-emerald-600 dark:text-emerald-400' : 'bg-amber-500/12 text-amber-600'"
                :title="detailHost.pushed
                  ? `Applied version ${detailHost.applied_version}${detailHost.applied_at ? ` on ${formatDateTime(detailHost.applied_at)}` : ''}`
                  : `Version ${detailHost.desired_version} published but not yet fetched/applied`"
              >{{ detailHost.pushed ? 'pushed' : 'not pushed' }}</span>
              <span
                class="inline-flex items-center gap-1.5 rounded-full border border-border/50 px-2 py-0.5 text-[11px] font-medium"
                :class="detailHost.live ? 'text-emerald-600 dark:text-emerald-400' : 'text-muted-foreground'"
                :title="detailHost.last_fetch_at ? `Last config fetch ${formatDateTime(detailHost.last_fetch_at)}` : 'sandboxd has never fetched config from this Manager'"
              >
                <span class="h-1.5 w-1.5 rounded-full" :class="detailHost.live ? 'bg-emerald-500' : 'bg-muted-foreground/40'" />
                {{ detailHost.live ? 'online' : 'offline' }}
              </span>
              <UiButton size="sm" variant="ghost" @click="detailHost = null"><X class="h-4 w-4" /></UiButton>
            </div>
          </div>
          <div class="mt-2.5 flex flex-wrap items-center gap-1.5">
            <span
              class="inline-flex items-baseline gap-1.5 rounded-lg px-2 py-0.5 text-[10.5px] tabular-nums"
              :class="detailHost.pushed ? 'border border-border/60' : 'bg-amber-500/12 text-amber-600'"
              :title="detailHost.pushed
                ? `Applied version ${detailHost.applied_version}${detailHost.applied_at ? ` on ${formatDateTime(detailHost.applied_at)}` : ''}`
                : `Version ${detailHost.desired_version} published but not yet fetched/applied`"
            >
              <span :class="detailHost.pushed ? 'text-muted-foreground' : 'text-amber-600/80'">applied</span>
              <span class="font-medium">v{{ detailHost.applied_version }} / v{{ detailHost.desired_version }}</span>
            </span>
            <span
              class="inline-flex items-baseline gap-1.5 rounded-lg border border-border/60 px-2 py-0.5 text-[10.5px] tabular-nums"
              :title="detailHost.last_fetch_at ? `Last config fetch ${formatDateTime(detailHost.last_fetch_at)}` : 'sandboxd has never fetched config from this Manager'"
            >
              <span class="text-muted-foreground">last fetch</span>
              <span class="font-medium">{{ detailHost.last_fetch_at ? formatDateTime(detailHost.last_fetch_at) : '—' }}</span>
            </span>
          </div>
        </div>

        <div class="min-h-0 flex-1 space-y-4 overflow-y-auto px-6 py-5">
          <!-- ── capacity: the host's slot budget ── -->
          <section class="space-y-2">
            <h4 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground">Capacity</h4>
            <div class="rounded-xl border border-border/50 p-3">
              <div class="flex flex-wrap items-center gap-x-2.5 gap-y-1.5">
                <span class="text-[12px] font-medium">Slot budget</span>
                <input
                  v-model.number="budgetDraft"
                  type="number"
                  min="0"
                  class="w-20 rounded-md border border-border/60 bg-background px-2 py-1 text-right text-xs tabular-nums outline-hidden focus:border-primary/50"
                  aria-label="Slot budget in slots (0 = inherit the platform default)"
                  @keydown.enter.prevent="saveBudget"
                />
                <span class="text-[11px] text-muted-foreground">slots</span>
                <span
                  v-if="normalizedBudget()"
                  class="rounded bg-emerald-500/10 px-1.5 py-px text-[9.5px] text-emerald-600 dark:text-emerald-400"
                >set on this host</span>
                <span v-else class="rounded bg-muted px-1.5 py-px text-[9.5px] text-muted-foreground">
                  inherits platform default · {{ platformDefaultSlots || 'unlimited' }}
                </span>
                <span class="ml-auto flex items-center gap-1.5">
                  <button
                    v-if="budgetDirty"
                    type="button"
                    class="text-[11px] text-muted-foreground transition-colors hover:text-foreground"
                    @click="cancelBudgetEdit"
                  >Cancel</button>
                  <UiButton v-if="budgetDirty" size="sm" :loading="savingBudget" @click="saveBudget">Save</UiButton>
                </span>
              </div>
              <p class="mt-1.5 text-[10px] leading-relaxed text-muted-foreground/60">
                Running jobs deduct their profile's <code class="mono">slots</code> here — <span class="font-mono">8</span> fits 8 small or 2 large jobs. <span class="font-mono">0</span> = platform default ({{ platformDefaultSlots || 'unlimited' }}) · effective immediately.
              </p>
            </div>
          </section>

          <!-- ── desired configuration ── -->
          <h4 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground">Desired configuration</h4>

          <div class="space-y-3 rounded-xl border border-border/50 p-4">
            <div class="flex items-baseline justify-between gap-2">
              <h5 class="text-[13px] font-semibold">Job sizes <span class="text-[11px] font-normal text-muted-foreground">(resource profiles)</span></h5>
              <button
                type="button"
                class="rounded px-1.5 py-0.5 text-[11px] text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
                @click="toggleRawJson"
              >
                {{ showRawJson ? '← Use form' : 'Raw JSON' }}
              </button>
            </div>

            <template v-if="!showRawJson">
              <div class="space-y-2">
                  <p class="type-meta">
                    Cards are what this host serves; the catalog (a standard ladder ships by default; <code class="mono-chip text-[10px]">default</code> and <code class="mono-chip text-[10px]">small</code> are fixed, the rest editable) lives in <button type="button" class="text-primary underline underline-offset-2" @click="openDefaultsDialog">Platform defaults</button>. Exclusions are per-machine — manage them below.
                  </p>
                  <div class="flex flex-wrap gap-2.5">
                    <!-- host-defined rows first: they override/diverge from the catalog -->
                    <div
                      v-for="row in formProfiles"
                      :key="'legacy-' + row.name"
                      class="relative w-44 cursor-pointer rounded-xl border border-amber-500/40 px-3 py-2.5 transition-colors hover:border-amber-500/70"
                      role="button"
                      tabindex="0"
                      :title="`Host-defined '${row.name}' (${row.name === 'default' ? 'fallback override' : 'not in the catalog'}) — click to stop defining it here`"
                      @click="dropLegacyRow(row)"
                      @keydown.enter.prevent="dropLegacyRow(row)"
                    >
                      <span class="flex items-center gap-1.5 text-[12px] font-semibold">
                        {{ row.name }}
                        <span class="rounded bg-amber-500/12 px-1 py-px text-[8.5px] font-medium text-amber-600">host-defined</span>
                      </span>
                      <span class="mt-0.5 block font-mono text-[9.5px] text-muted-foreground">{{ specLine(row) }}</span>
                      <span class="mt-1 block text-[9.5px] text-muted-foreground/70">click to drop the host definition</span>
                    </div>
                    <!-- offered catalog profiles: solid card + check = on; click excludes -->
                    <div
                      v-for="name in offeredCatalogSizes"
                      :key="name"
                      class="relative w-44 cursor-pointer rounded-xl border border-border bg-background px-3 py-2.5 transition-colors hover:border-primary/50"
                      role="button"
                      tabindex="0"
                      :title="`Offering '${name}' — click to exclude it from this host`"
                      @click="toggleCatalogSize(name)"
                      @keydown.enter.prevent="toggleCatalogSize(name)"
                    >
                  <span class="flex items-center gap-1.5 text-[12px] font-semibold">
                    <Check class="h-3 w-3 shrink-0 text-primary" aria-hidden="true" />
                    {{ name }}
                    <span
                      class="rounded px-1 py-px text-[8.5px] font-medium"
                      :class="sizeGrantLabel(name).granted ? 'bg-emerald-500/12 text-emerald-600' : 'bg-amber-500/12 text-amber-600'"
                    >{{ sizeGrantLabel(name).text }}</span>
                  </span>
                  <span
                    v-if="name === defaultAliasName"
                    class="mt-0.5 inline-flex items-center rounded bg-primary/10 px-1 py-px text-[8.5px] font-medium text-primary"
                    title="'default' shares this envelope — jobs without runner.resources run here"
                  >default</span>
                  <span class="mt-0.5 block font-mono text-[9.5px] text-muted-foreground">{{ specLine(catalogSizeRow(name)) }}</span>
                      <span class="mt-1 block text-[9.5px] text-muted-foreground/70">{{ sizeGrantLabel(name).granted ? `grant: ${sizeGrantLabel(name).text}` : 'no team may request it yet' }}</span>
                    </div>
                  </div>
                  <p v-if="!offeredCatalogSizes.length && !formProfiles.length" class="text-[10px] leading-relaxed text-amber-600">
                    This host excludes every catalog entry — only the <code class="mono">default</code> fallback applies to jobs here. Re-offer entries under <strong>Manage catalog profiles</strong>.
                  </p>
                  <!-- catalog exclusions: the exception list lives behind one manage entry -->
                  <div>
                    <UiButton size="sm" variant="ghost" class="-ml-2" @click="showCatalogPanel = !showCatalogPanel">
                      <ChevronRight class="h-3.5 w-3.5 transition-transform" :class="showCatalogPanel && 'rotate-90'" />
                      Manage catalog profiles
                      <span v-if="excludedCatalogSizes.length" class="font-normal text-muted-foreground">— {{ excludedCatalogSizes.length }} excluded</span>
                    </UiButton>
                    <div v-if="showCatalogPanel" class="mt-1.5 space-y-0.5 rounded-lg border border-border/50 p-1.5">
                      <p class="type-meta px-1 pt-0.5">Checked = offered. Uncheck to exclude; excluded entries sit dimmed at the bottom until re-checked.</p>
                      <label
                        v-for="name in [...offeredCatalogSizes, ...excludedCatalogSizes]"
                        :key="name"
                        class="flex cursor-pointer items-center gap-2 rounded-md px-1.5 py-1 transition-colors hover:bg-muted/40"
                        :class="excludedSizes.has(name) && 'opacity-60'"
                      >
                        <input
                          type="checkbox"
                          class="h-3.5 w-3.5 shrink-0 accent-primary"
                          :checked="!excludedSizes.has(name)"
                          @change="toggleCatalogSize(name)"
                        />
                        <span class="min-w-0 flex-1">
                          <span class="flex items-center gap-1.5 text-[12px] font-medium">
                            {{ name }}
                            <span
                              class="rounded px-1 py-px text-[8.5px] font-medium"
                              :class="sizeGrantLabel(name).granted ? 'bg-emerald-500/12 text-emerald-600' : 'bg-amber-500/12 text-amber-600'"
                            >{{ sizeGrantLabel(name).text }}</span>
                          </span>
                          <span
                            v-if="name === defaultAliasName"
                            class="mt-0.5 inline-flex items-center rounded bg-primary/10 px-1 py-px text-[8.5px] font-medium text-primary"
                          >default</span>
                          <span class="block font-mono text-[9.5px] text-muted-foreground">{{ specLine(catalogSizeRow(name)) }}</span>
                        </span>
                      </label>
                    </div>
                  </div>
                </div>

                <!-- footnotes: grouped, separated from the editable surface -->
                <div class="space-y-1 border-t border-border/40 pt-2.5">
                  <p v-if="fallbackSize" class="text-[10px] leading-relaxed text-muted-foreground">
                    Fallback envelope <code class="mono">default</code><template v-if="defaultAliasName"> (= <code class="mono">{{ defaultAliasName }}</code>)</template> — <span class="font-mono">{{ specLine(fallbackSize) }}</span> — runs implicitly for every job without <code class="mono">runner.resources</code>; every team may use it.
                  </p>
                  <p class="text-[10px] leading-relaxed text-muted-foreground">
                    “no grant” = the profile is offered here but no team may request it — grant it under <strong>Profile grants</strong> on the page above. Disk is a fixed tmpfs and GPU passthrough is not yet supported — those follow platform evolution.
                  </p>
                </div>
            </template>

            <template v-else>
              <p class="type-meta">The effective document — the host's own rows plus the catalog rows it offers, exactly what sandboxd fetches. Applying treats catalog-identical rows as inherited (not host-pinned); switching back to the form applies it (validated there before you can publish), “Apply to form” does the same without leaving this view.</p>
              <textarea
                v-model="rawJsonText"
                rows="14"
                spellcheck="false"
                class="w-full font-mono text-[11px] rounded-lg border border-border/60 bg-background p-2"
              ></textarea>
              <UiButton size="sm" variant="ghost" @click="applyRawJson"><Check class="h-3.5 w-3.5" /> Apply to form</UiButton>
            </template>
          </div>

          <!-- ── version history: reference material, folded by default ── -->
          <div v-if="versionsLoading || versions.length" class="overflow-hidden rounded-xl border border-border/50">
            <button
              type="button"
              class="flex w-full items-center gap-1.5 px-3 py-2 text-left text-[12px] font-medium transition-colors hover:bg-muted/50"
              :aria-expanded="showVersionHistory"
              @click="showVersionHistory = !showVersionHistory"
            >
              <ChevronRight class="h-3.5 w-3.5 transition-transform" :class="showVersionHistory && 'rotate-90'" />
              Version history
              <span class="font-normal text-muted-foreground">· {{ versionsLoading ? '…' : versions.length }}</span>
              <span class="ml-auto text-[10px] font-normal tabular-nums text-muted-foreground">v{{ detailHost.desired_version }} desired · v{{ detailHost.applied_version }} applied</span>
            </button>
            <div v-if="showVersionHistory" class="border-t border-border/50">
              <div v-if="versionsLoading" class="px-3 py-2 type-meta">Loading…</div>
              <template v-else>
                <ul class="divide-y divide-border/40 text-xs">
                  <li
                    v-for="(v, i) in pagedVersions"
                    :key="v.version"
                    class="grid grid-cols-[2.5rem_minmax(0,1fr)_6.25rem_10rem_4.5rem] items-baseline gap-x-2 px-3 py-1.5"
                  >
                    <span class="font-mono text-[11px] text-muted-foreground">v{{ v.version }}</span>
                    <span class="min-w-0 break-words">{{ versionEvent(v, versions[(versionPage - 1) * VERSIONS_PAGE_SIZE + i + 1]) }}</span>
                    <span class="type-meta whitespace-nowrap">{{ formatDateTime(v.created_at) }}</span>
                    <span class="min-w-0 break-words type-meta">{{ v.created_by }}</span>
                    <span v-if="v.version === detailHost.desired_version" class="justify-self-end rounded bg-muted px-1 py-px text-[9px] text-muted-foreground">current</span>
                    <UiButton v-else size="sm" variant="ghost" class="justify-self-end" @click="rollbackTo(v.version)">Restore</UiButton>
                  </li>
                </ul>
                <div v-if="versionPageCount > 1" class="flex items-center justify-between border-t border-border/40 px-3 py-2">
                  <UiButton size="sm" variant="ghost" class="-ml-2" :disabled="versionPage <= 1" @click="gotoVersionPage(versionPage - 1)">
                    <ChevronRight class="h-3.5 w-3.5 rotate-180" /> Prev
                  </UiButton>
                  <span class="type-meta">Page {{ versionPage }} of {{ versionPageCount }}</span>
                  <UiButton size="sm" variant="ghost" class="-mr-2" :disabled="versionPage >= versionPageCount" @click="gotoVersionPage(versionPage + 1)">
                    Next <ChevronRight class="h-3.5 w-3.5" />
                  </UiButton>
                </div>
              </template>
            </div>
          </div>
        </div>

        <!-- ── dock: feedback + actions stay visible while the body scrolls ── -->
        <div class="shrink-0 border-t border-border/50 px-6 py-3">
          <div class="flex items-center gap-3">
            <div class="min-w-0 flex-1">
              <div v-if="configProblems.length || configWarnings.length || configValid" class="max-h-24 space-y-1 overflow-y-auto">
                <ul v-if="configProblems.length" class="list-inside list-disc space-y-0.5 text-xs text-destructive">
                  <li v-for="p in configProblems" :key="p">{{ p }}</li>
                </ul>
                <ul v-if="configWarnings.length" class="list-inside list-disc text-xs text-amber-600">
                  <li v-for="w in configWarnings" :key="w">{{ w }}</li>
                </ul>
                <p v-if="configValid && !configProblems.length" class="flex items-start gap-1 text-xs text-emerald-600">
                  <Check class="mt-0.5 h-3.5 w-3.5 shrink-0" />
                  <span>
                    Checks passed — profiles, tier images (digest-pinned) and network are accepted. Publish stages this as the next desired version; sandboxd picks it up on its next fetch.
                  </span>
                </p>
              </div>
              <p v-else class="type-meta truncate">Validate checks profiles, tier images and network before publishing.</p>
            </div>
            <div class="flex shrink-0 gap-2">
              <UiButton size="sm" variant="ghost" :disabled="savingConfig" @click="validateConfig">
                <ShieldCheck class="h-3.5 w-3.5" /> Validate
              </UiButton>
              <UiButton size="sm" :loading="savingConfig" @click="saveConfig">Publish</UiButton>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- ── platform defaults dialog ── -->
    <div v-if="showDefaultsDialog" class="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4" @click.self="showDefaultsDialog = false">
      <div class="bg-card rounded-xl border border-border/60 w-full max-w-2xl max-h-[85vh] overflow-y-auto shadow-lg">
        <div class="flex items-start justify-between gap-3 border-b border-border/50 px-6 py-4">
          <div class="min-w-0">
            <h3 class="text-base font-semibold tracking-tight">Platform defaults</h3>
            <p class="mt-0.5 type-meta">
              Saving bumps the desired version of every host whose effective document changes, so hosts re-fetch.
            </p>
          </div>
          <UiButton size="sm" variant="ghost" @click="showDefaultsDialog = false"><X class="h-4 w-4" /></UiButton>
        </div>

        <div class="space-y-3 px-6 py-5">
          <!-- ── isolation levels: tier envelope images ── -->
          <div class="rounded-lg border border-border/60 p-3.5">
            <div class="mb-2.5">
              <div class="text-[12.5px] font-semibold">Isolation levels</div>
              <p class="mt-0.5 text-[10.5px] text-muted-foreground/80">
                The envelope image each level runs in — leave a row empty to provide no default for that level (jobs selecting it fail closed until an image exists).
              </p>
            </div>
            <div class="grid grid-cols-[92px_minmax(0,1fr)_130px_32px] gap-2 pb-1 text-[9.5px] font-semibold uppercase tracking-wide text-muted-foreground">
              <span>Tier</span><span>Envelope image</span><span>Runtime</span><span></span>
            </div>
            <div class="divide-y divide-border/40">
              <div
                v-for="row in defaultsRows"
                :key="row.name"
                class="grid grid-cols-[92px_minmax(0,1fr)_130px_32px] items-center gap-2 py-1.5"
              >
                <span class="justify-self-start rounded-md bg-muted px-2 py-1 font-mono text-[11px] font-medium">{{ row.name }}</span>
                <div class="flex min-w-0 items-center gap-1.5">
                  <input
                    v-model="row.image"
                    :placeholder="TIER_IMAGE_HINTS[row.name] ?? `no default for ${row.name}`"
                    class="min-w-0 flex-1 rounded-md border border-border/60 bg-background px-2 py-1 font-mono text-xs outline-hidden focus:border-primary/50"
                  />
                  <span v-if="isPinnedImage(row.image)" class="inline-flex shrink-0 items-center gap-1 rounded bg-emerald-500/10 px-1.5 py-0.5 text-[9.5px] text-emerald-600 dark:text-emerald-400">
                    <Check class="h-2.5 w-2.5" /> digest pinned
                  </span>
                </div>
                <input
                  v-model="row.runtime"
                  list="oci-runtime-options"
                  placeholder="default (runc)"
                  class="w-full rounded-md border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50"
                />
                <UiButton
                  size="sm"
                  variant="ghost"
                  class="h-8 w-8 justify-self-start"
                  :loading="resolvingTier === row.image.trim()"
                  :disabled="!row.image.trim() || isPinnedImage(row.image) || resolvingTier !== null"
                  title="Resolve this tag to a digest now (Save also does this)"
                  @click="resolveDefaultsRow(row)"
                ><RefreshCw class="h-4 w-4" /></UiButton>
              </div>
            </div>
            <p class="mt-1.5 text-[9.5px] text-muted-foreground/70">Runtime empty = daemon default (runc).</p>
          </div>

          <!-- ── job sizes: the resource-profile catalog ── -->
          <div class="rounded-lg border border-border/60 p-3.5">
            <div class="mb-2.5">
              <div class="text-[12.5px] font-semibold">Job sizes</div>
              <p class="mt-0.5 text-[10.5px] text-muted-foreground/80">
                The resource-profile catalog — it ships with a standard ladder (small / medium / large, each doubling the last). <code class="mono-chip text-[10px]">small</code> is mandatory and doubles as <code class="mono-chip text-[10px]">default</code> — requests for the fallback envelope run with small's numbers, kept in lockstep on save; the rest of the ladder is editable — add, renumber, or delete freely. Every host offers all of it unless it excludes entries on its own page; pipelines request one via <code class="mono-chip text-[10px]">runner.resources: &lt;name&gt;</code>.
              </p>
            </div>
            <div class="grid grid-cols-[92px_repeat(4,minmax(0,1fr))_32px] gap-2 pb-1 text-[9.5px] font-semibold uppercase tracking-wide text-muted-foreground">
              <span>Name</span><span>Memory</span><span>CPU</span><span>Pids</span><span>Slots</span><span></span>
            </div>
            <div class="divide-y divide-border/40">
              <div
                v-for="(row, i) in defaultProfileRows"
                :key="i"
                class="grid grid-cols-[92px_repeat(4,minmax(0,1fr))_32px] items-center gap-2 py-1.5"
              >
                <div class="flex min-w-0 flex-col">
                  <input
                    v-model="row.name"
                    :disabled="row.name === 'small'"
                    placeholder="large"
                    class="w-full rounded-md border border-border/60 bg-background px-2 py-1 font-mono text-xs outline-hidden focus:border-primary/50 disabled:opacity-70"
                  />
                </div>
                <input v-model="row.memory" placeholder="2g" class="w-full rounded-md border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50" />
                <input v-model="row.cpu" placeholder="2" class="w-full rounded-md border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50" />
                <input v-model.number="row.pids" type="number" min="1" class="w-full rounded-md border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50" />
                <input v-model.number="row.slots" type="number" min="1" max="32" class="w-full rounded-md border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50" />
                <UiButton
                  v-if="row.name !== 'small'"
                  size="sm"
                  variant="ghost"
                  class="h-8 w-8 text-muted-foreground hover:bg-red-500/10 hover:text-red-500"
                  :title="`Remove ${row.name || '(unnamed)'}`"
                  @click="removeDefaultProfileRow(i)"
                ><Trash2 class="h-4 w-4" /></UiButton>
                <!-- small is mandatory: reserve the delete-button cell -->
                <span v-else class="h-8 w-8" aria-hidden="true"></span>
              </div>
            </div>
            <UiButton size="sm" variant="ghost" class="-ml-2 mt-1" @click="addDefaultProfileRow"><Plus class="h-3.5 w-3.5" /> Add job size</UiButton>
          </div>

          <!-- ── platform-wide values hosts inherit ── -->
          <div class="rounded-lg border border-border/60 p-3.5">
            <div class="mb-2.5">
              <div class="text-[12.5px] font-semibold">Inherited by every host</div>
              <p class="mt-0.5 text-[10.5px] text-muted-foreground/80">
                Platform-wide values — each host's own document overrides them.
              </p>
            </div>
            <div class="grid grid-cols-2 gap-4">
              <label class="block">
                <span class="mb-1 block text-[9.5px] font-semibold uppercase tracking-wide text-muted-foreground">Docker network</span>
                <input
                  v-model="platformDefaultNetwork"
                  list="docker-network-options"
                  placeholder="bridge"
                  class="w-full rounded-md border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50"
                />
                <span class="mt-1 block text-[9.5px] text-muted-foreground/70">Inherited by hosts whose document leaves it unset — custom host networks can be typed.</span>
              </label>
              <label class="block">
                <span class="mb-1 block text-[9.5px] font-semibold uppercase tracking-wide text-muted-foreground">Host slot budget <span class="font-normal normal-case tracking-normal">· 0 = unlimited</span></span>
                <input
                  v-model.number="platformDefaultSlots"
                  type="number"
                  min="0"
                  class="w-full rounded-md border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50"
                  title="Max slots across every runner on a host that hasn't set its own; 0 = unlimited"
                />
                <span class="mt-1 block text-[9.5px] text-muted-foreground/70">Fleet-wide cap for hosts without their own budget.</span>
              </label>
            </div>
          </div>

          <ul v-if="defaultsProblems.length" class="text-xs text-destructive list-disc list-inside space-y-0.5">
            <li v-for="p in defaultsProblems" :key="p">{{ p }}</li>
          </ul>
        </div>

        <datalist id="oci-runtime-options">
          <option value="runc">runc — standard OCI runtime</option>
          <option value="crun">crun — lightweight OCI runtime</option>
          <option value="runsc">runsc — gVisor userspace kernel</option>
          <option value="kata-fc">kata-fc — Kata Firecracker microVM</option>
          <option value="kata-qemu">kata-qemu — Kata QEMU microVM</option>
          <option value="sysbox-runc">sysbox-runc — Sysbox container sandbox</option>
        </datalist>

        <datalist id="docker-network-options">
          <option value="bridge">bridge — default NAT network</option>
          <option value="host">host — share the host's network stack</option>
          <option value="none">none — no networking</option>
        </datalist>

        <div class="flex justify-end gap-2 border-t border-border/50 px-6 py-3.5">
          <UiButton size="sm" variant="ghost" @click="showDefaultsDialog = false">Cancel</UiButton>
          <UiButton size="sm" :loading="savingDefaults" @click="saveDefaults">Save defaults</UiButton>
        </div>
      </div>
    </div>

    <!-- ── enroll / token dialog ── -->
    <div v-if="showEnroll" class="fixed inset-0 z-50 flex items-center justify-center bg-black/40" @click.self="showEnroll = false">
      <div class="bg-card rounded-xl border border-border/60 p-5 w-full max-w-md space-y-3 shadow-lg">
        <template v-if="!issuedToken">
          <div class="flex items-center gap-2">
            <h3 class="min-w-0 truncate text-[13.5px] font-medium">Enroll sandbox host</h3>
            <UiButton size="sm" variant="ghost" class="-mr-2 ml-auto" @click="showEnroll = false"><X class="h-4 w-4" /></UiButton>
          </div>
          <p class="text-[11px] leading-relaxed text-muted-foreground">
            This reserves an identity for a <strong class="font-medium text-foreground">buz-sandboxd</strong> daemon so it can fetch its configuration from this Manager.
            Enrolling mints a <strong class="font-medium text-foreground">one-time token</strong> — you'll paste a small env block on that machine next.
          </p>

          <label class="block">
            <span class="mb-1 block text-xs font-medium">Host ID</span>
            <input v-model="enrollHostId" list="known-host-ids" placeholder="web-1" class="w-full text-sm rounded-lg border border-border/60 bg-background px-3 py-2" @input="enrollError = ''" />
          </label>
          <p class="text-[10.5px] leading-relaxed text-muted-foreground">
            A name for the <span class="text-foreground">machine running Docker</span> — your choice (hostname, inventory name…), just keep it consistent. A daemon identity <span class="font-mono text-foreground">sandboxd-&lt;host&gt;</span> is derived from it.
          </p>
          <p v-if="enrollHostId.trim() && !knownHostIds.includes(enrollHostId.trim())" class="type-meta">
            New host <span class="font-mono">{{ enrollHostId.trim() }}</span> — its identity is reserved now; nothing is installed yet.
          </p>
          <p v-else-if="enrollHostId.trim() && instancesOnHost.length" class="type-meta">
            <span class="font-mono">{{ enrollHostId.trim() }}</span> already has {{ instancesOnHost.length }} instance(s) — this adds another daemon on it.
          </p>
          <div v-if="knownHostIds.length" class="flex flex-wrap items-center gap-1.5 text-[10.5px] text-muted-foreground">
            Existing hosts:
            <button
              v-for="h in knownHostIds"
              :key="h"
              type="button"
              class="rounded-full border border-border/60 px-2 py-px font-mono text-[10px] transition-colors hover:bg-muted"
              :title="`Use ${h}`"
              @click="enrollHostId = h"
            >{{ h }}</button>
          </div>
          <datalist id="known-host-ids">
            <option v-for="h in knownHostIds" :key="h" :value="h" />
          </datalist>

          <label class="block">
            <span class="mb-1 flex items-center justify-between text-xs font-medium">
              Slot budget
              <span class="font-normal text-[10px] text-muted-foreground">0 = inherit platform default ({{ platformDefaultSlots || 'unlimited' }})</span>
            </span>
            <div class="flex items-center gap-2">
              <input v-model.number="enrollSlots" type="number" min="0" class="w-24 rounded-lg border border-border/60 bg-background px-3 py-2 text-sm tabular-nums outline-hidden focus:border-primary/50" />
              <span class="text-[11px] text-muted-foreground">slots</span>
            </div>
            <span class="mt-1 block text-[10.5px] leading-relaxed text-muted-foreground">
              Concurrent capacity of this machine — running jobs deduct their profile's slots from it. Adjustable later in the host's Capacity panel.
            </span>
          </label>

          <div class="pt-1">
            <button type="button" class="inline-flex items-center gap-1 text-xs text-muted-foreground transition-colors hover:text-foreground" @click="showAdvanced = !showAdvanced">
              <ChevronRight class="h-3.5 w-3.5 transition-transform" :class="showAdvanced && 'rotate-90'" />
              Advanced
            </button>
            <span class="ml-1 text-[10px] italic text-muted-foreground/70">instance ID defaults to sandboxd-&lt;host&gt;</span>
            <div v-if="showAdvanced" class="mt-2 space-y-3 border-l border-border/40 pl-3">
              <label class="block">
                <span class="mb-1 block text-xs font-medium">Instance ID</span>
                <input v-model="enrollInstanceId" placeholder="sandboxd-web-1" class="w-full text-sm rounded-lg border border-border/60 bg-background px-3 py-2" @input="instanceEdited = true; enrollError = ''" />
              </label>
              <p class="type-meta">One machine can run several daemons (rootful + rootless Docker, canary…) — each gets its own instance ID and token. Adjustable later in the host's detail dialog too.</p>
            </div>
          </div>
          <p v-if="enrollError" class="text-xs text-red-500">{{ enrollError }}</p>
          <div class="flex justify-end gap-2 pt-1">
            <UiButton size="sm" variant="ghost" @click="showEnroll = false">Cancel</UiButton>
            <UiButton size="sm" :loading="creating" :disabled="!enrollHostId.trim() || !enrollInstanceId.trim()" @click="handleEnroll">Enroll &amp; get token</UiButton>
          </div>
        </template>
        <template v-else>
          <div class="flex items-center gap-2">
            <h3 class="min-w-0 truncate text-[13.5px] font-medium"><KeyRound class="mr-1 inline h-4 w-4" />One-time token — deploy on {{ enrollHostId.trim() || 'the machine' }}</h3>
            <UiButton size="sm" variant="ghost" class="-mr-2 ml-auto" @click="showEnroll = false"><X class="h-4 w-4" /></UiButton>
          </div>
          <div class="flex items-start gap-2 rounded-lg bg-amber-500/12 px-2.5 py-2 text-[10.5px] leading-relaxed text-amber-600">
            <AlertTriangle class="mt-0.5 h-3 w-3 shrink-0" />
            <span>Shown <strong class="font-medium">only now</strong> — the Manager stores just its SHA-256. Lost it? <strong class="font-medium">Rotate token</strong> on the host card re-issues one.</span>
          </div>
          <ol class="list-none space-y-1.5 text-[11px] leading-relaxed">
            <li class="flex items-baseline gap-2">
              <span class="inline-flex h-4 w-4 shrink-0 translate-y-0.5 items-center justify-center rounded-full bg-muted text-[9px] text-muted-foreground">1</span>
              <span class="min-w-0">Copy the env block below and run <strong class="font-medium">buz-sandboxd</strong> on {{ enrollHostId.trim() || 'the machine' }} with it
                <UiButton size="sm" variant="ghost" class="-ml-1" @click="copySnippet"><Copy class="h-3 w-3" /> {{ snippetCopied ? 'Copied' : 'Copy env' }}</UiButton>
                <UiButton size="sm" variant="ghost" @click="copyToken"><Copy class="h-3 w-3" /> {{ tokenCopied ? 'Copied' : 'Token only' }}</UiButton>
              </span>
            </li>
            <li class="flex items-baseline gap-2">
              <span class="inline-flex h-4 w-4 shrink-0 translate-y-0.5 items-center justify-center rounded-full bg-muted text-[9px] text-muted-foreground">2</span>
              <span>It fetches config from this Manager on startup — no other setup needed.</span>
            </li>
            <li class="flex items-baseline gap-2">
              <span class="inline-flex h-4 w-4 shrink-0 translate-y-0.5 items-center justify-center rounded-full bg-muted text-[9px] text-muted-foreground">3</span>
              <span>The host card turns <span class="font-medium text-emerald-600 dark:text-emerald-400">online</span> here within seconds; until then it shows as enrolled.</span>
            </li>
          </ol>
          <pre class="max-h-48 overflow-auto whitespace-pre-wrap rounded-lg border border-border/60 bg-background p-3 font-mono text-[10.5px] leading-relaxed">{{ bootstrapSnippet }}</pre>
          <div class="space-y-0.5">
            <div class="grid grid-cols-[10.5rem_1fr] items-baseline gap-2">
              <span class="truncate font-mono text-[9px] text-muted-foreground">BUZ_HOST_ID</span>
              <span class="text-[10px] text-muted-foreground">the machine's identity — what you just enrolled</span>
            </div>
            <div class="grid grid-cols-[10.5rem_1fr] items-baseline gap-2">
              <span class="truncate font-mono text-[9px] text-muted-foreground">BUZ_SANDBOXD_INSTANCE_ID</span>
              <span class="text-[10px] text-muted-foreground">which daemon on that machine (one per Docker daemon)</span>
            </div>
            <div class="grid grid-cols-[10.5rem_1fr] items-baseline gap-2">
              <span class="truncate font-mono text-[9px] text-muted-foreground">BUZ_SANDBOXD_TOKEN</span>
              <span class="text-[10px] text-muted-foreground">its one-time credential for this Manager</span>
            </div>
            <div class="grid grid-cols-[10.5rem_1fr] items-baseline gap-2">
              <span class="truncate font-mono text-[9px] text-muted-foreground">BUZ_MANAGER_URL</span>
              <span class="text-[10px] text-muted-foreground">this Manager's address</span>
            </div>
          </div>
          <p class="text-[10px] leading-relaxed text-muted-foreground">
            Production tip: keep the token out of <code class="mono">docker inspect</code> — write it to a root-owned file and point <code class="mono">BUZ_SANDBOXD_TOKEN_FILE</code> at it (mode 0400). The env var above is the quick start.
          </p>
          <div class="flex justify-end pt-1">
            <UiButton size="sm" @click="showEnroll = false">Done</UiButton>
          </div>
        </template>
      </div>
    </div>
  </div>
  </div>
</template>
