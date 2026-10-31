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

// ── profile grants: profile-centric rules table (fleet-wide allowlist) ──
const grantError = ref('')
const grantSaving = ref(false)
/** Inline "add team" panel state: which profile row is open, selected team. */
const grantPanelProfile = ref('')
const grantPanelTeamId = ref('')

const PROFILE_RE = /^[a-z0-9][a-z0-9-]{0,31}$/

/** One row per profile that appears in the grants table: granted profiles
 *  first (with their teams), then offered-but-ungranted ones (warning). */
interface GrantProfileRow {
  profile: string
  teams: { teamId: string; teamName: string; grant: ResourceGrant }[]
  /** host_ids whose effective document offers this profile */
  offeredBy: string[]
}

/** Profiles offered by each host come straight from the list API, whose
 *  document is already the *effective* one (platform defaults merged) — a
 *  single source of truth shared with dispatch. */
function offeredProfilesFor(host: SandboxHost): Set<string> {
  return new Set(Object.keys(host.document?.profiles ?? {}))
}

const grantProfileRows = computed<GrantProfileRow[]>(() => {
  const byProfile = new Map<string, GrantProfileRow>()
  for (const g of grants.value) {
    let row = byProfile.get(g.profile)
    if (!row) {
      row = { profile: g.profile, teams: [], offeredBy: [] }
      byProfile.set(g.profile, row)
    }
    row.teams.push({ teamId: g.team_id, teamName: g.team_name, grant: g })
  }
  // Profiles offered by any host but absent from grants — the "exists here
  // but no team can request it" warning rows.
  for (const host of hosts.value) {
    for (const name of offeredProfilesFor(host)) {
      if (name === 'default') continue
      if (!byProfile.has(name)) {
        byProfile.set(name, { profile: name, teams: [], offeredBy: [] })
      }
    }
  }
  const rows = [...byProfile.values()]
  for (const row of rows) {
    row.teams.sort((a, b) => a.teamName.localeCompare(b.teamName))
    row.offeredBy = hosts.value
      .filter((h) => offeredProfilesFor(h).has(row.profile))
      .map((h) => h.host_id)
      .sort()
  }
  return rows.sort((a, b) => {
    // granted first, then alphabetical
    const aHas = a.teams.length > 0 ? 0 : 1
    const bHas = b.teams.length > 0 ? 0 : 1
    return aHas - bHas || a.profile.localeCompare(b.profile)
  })
})

/** Teams that do not yet hold a grant for the given profile. */
function grantableTeams(profile: string): Team[] {
  const held = new Set(
    grants.value.filter((g) => g.profile === profile).map((g) => g.team_id),
  )
  return teams.value.filter((t) => !held.has(t.id))
}

function openGrantPanel(profile: string) {
  grantPanelProfile.value = profile
  grantPanelTeamId.value = ''
  grantError.value = ''
}

function closeGrantPanel() {
  grantPanelProfile.value = ''
  grantPanelTeamId.value = ''
  grantError.value = ''
}

async function addGrant(profile: string) {
  grantError.value = ''
  const teamId = grantPanelTeamId.value
  if (!teamId) return
  if (profile === 'default') {
    grantError.value = "'default' is implicitly available to every team — grant a non-default profile."
    return
  }
  if (!PROFILE_RE.test(profile)) {
    grantError.value = 'Profile name must match [a-z0-9][a-z0-9-]{0,31}.'
    return
  }
  if (grants.value.some((g) => g.team_id === teamId && g.profile === profile)) {
    grantError.value = `${teamName(teamId)} already has '${profile}'.`
    return
  }
  grantSaving.value = true
  try {
    await sandboxHostsApi.grant(teamId, profile)
    closeGrantPanel()
    await load()
  } catch (e) {
    console.error(e)
    await dialog.alert('Failed to add grant')
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
    await sandboxHostsApi.revokeGrant(grant.team_id, grant.profile)
    await load()
  } catch (e) {
    console.error(e)
    await dialog.alert('Revoke failed')
  }
}

/** New-profile grant (bottom row): pick an offered profile from a dropdown. */
const newGrantTeamId = ref('')
const newGrantProfile = ref('')

/** Every non-default profile name any host effectively offers — the dropdown
 *  candidates (platform defaults + host overrides). Ones already fully listed
 *  in the table still appear so an operator can grant a second team from
 *  here without hunting the row. */
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

async function addNewProfileGrant() {
  grantError.value = ''
  const profile = newGrantProfile.value.trim()
  const teamId = newGrantTeamId.value
  if (!teamId || !profile) return
  if (profile === 'default') {
    grantError.value = "'default' is implicitly available to every team — grant a non-default profile."
    return
  }
  if (!PROFILE_RE.test(profile)) {
    grantError.value = 'Profile name must match [a-z0-9][a-z0-9-]{0,31}.'
    return
  }
  if (grants.value.some((g) => g.team_id === teamId && g.profile === profile)) {
    grantError.value = `${teamName(teamId)} already has '${profile}'.`
    return
  }
  grantSaving.value = true
  try {
    await sandboxHostsApi.grant(teamId, profile)
    newGrantProfile.value = ''
    newGrantTeamId.value = ''
    await load()
  } catch (e) {
    console.error(e)
    await dialog.alert('Failed to add grant')
  } finally {
    grantSaving.value = false
  }
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
const savingConfig = ref(false)
const configProblems = ref<string[]>([])
const configWarnings = ref<string[]>([])
const configValid = ref(false)
const versions = ref<ConfigVersion[]>([])
const versionsLoading = ref(false)

/** Structured form state — mirrors shared::sandbox_config::validate rules. */
type ProfileRow = { name: string; memory: string; cpu: string; pids: number; slots: number }
type TierRow = { name: string; image: string; runtime: string }
const formProfiles = ref<ProfileRow[]>([])
const formTiers = ref<TierRow[]>([])
const formNetwork = ref('')

const showRawJson = ref(false)
const rawJsonText = ref('')
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
    .filter((row) => row.name === 'default' || !sameAsCatalog(row))
  excludedSizes.value = new Set(
    Object.keys(platformDefaultProfiles.value).filter(
      (name) => name !== 'default' && !(name in (doc.profiles ?? {})),
    ),
  )
  formTiers.value = Object.entries(doc.tiers ?? {}).map(([name, t]) => ({
    name,
    image: String(t?.image ?? ''),
    runtime: String(t?.runtime ?? ''),
  }))
  formNetwork.value = String(doc.network ?? '')
}

// A validation verdict only describes the values it ran on.
watch([formProfiles, formTiers, formNetwork], () => {
  configValid.value = false
}, { deep: true })

// ── Job sizes: a selection surface over the platform catalog ──
/** Catalog profiles (non-default) as toggleable chips: a host offers every
 *  catalog entry unless it explicitly excludes it (exclude_profiles). */
const catalogSizes = computed(() =>
  Object.keys(platformDefaultProfiles.value)
    .filter((name) => name !== 'default')
    .sort(),
)
/** Catalog profile names this host refuses — persisted as exclude_profiles. */
const excludedSizes = ref<Set<string>>(new Set())

function toggleCatalogSize(name: string) {
  const next = new Set(excludedSizes.value)
  if (next.has(name)) next.delete(name)
  else next.add(name)
  excludedSizes.value = next
}

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

/** Loose dump for the raw-JSON escape hatch (no validation). */
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
  const tiers: Record<string, { image: string; runtime: string }> = {}
  formTiers.value.forEach((row, i) => {
    tiers[row.name.trim() || `unnamed-${i + 1}`] = { image: row.image, runtime: row.runtime }
  })
  return {
    profiles,
    exclude_profiles: [...excludedSizes.value].sort(),
    tiers,
    network: formNetwork.value,
  }
}

function toggleRawJson() {
  if (!showRawJson.value) {
    rawJsonText.value = JSON.stringify(dumpDocumentLoose(), null, 2)
    showRawJson.value = true
  } else {
    showRawJson.value = false
  }
}

/** Paste JSON → parse into the form; problems land in the red list. */
function applyRawJson() {
  let parsed: unknown
  try {
    parsed = JSON.parse(rawJsonText.value)
  } catch (e) {
    configProblems.value = [`invalid JSON: ${e instanceof Error ? e.message : String(e)}`]
    return
  }
  const obj = (parsed ?? {}) as Record<string, unknown>
  const profilesObj = (obj.profiles ?? {}) as Record<string, Record<string, unknown>>
  const tiersObj = (obj.tiers ?? {}) as Record<string, Record<string, unknown>>
  excludedSizes.value = new Set(
    ((obj.exclude_profiles ?? []) as unknown[]).map(String).filter((n) => n !== 'default'),
  )
  formProfiles.value = Object.entries(profilesObj).map(([name, p]) => ({
    name,
    memory: String(p?.memory ?? ''),
    cpu: String(p?.cpu ?? ''),
    pids: Number(p?.pids) || 0,
    slots: Number(p?.slots) || 0,
  }))
  formTiers.value = Object.entries(tiersObj).map(([name, t]) => ({
    name,
    image: String(t?.image ?? ''),
    runtime: String(t?.runtime ?? ''),
  }))
  formNetwork.value = String(obj.network ?? '')
  showRawJson.value = false
  configValid.value = false
  configProblems.value = validateRows().problems
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
  // Default profile rows: 'default' first (required), then the catalog.
  const catalog = { ...platformDefaultProfiles.value }
  const defaultProfile = catalog.default ?? { memory: '2g', cpu: '2', pids: 256, slots: 1 }
  delete catalog.default
  defaultProfileRows.value = [
    { name: 'default', ...defaultProfile },
    ...Object.entries(catalog).map(([name, p]) => ({
      name,
      memory: p.memory,
      cpu: p.cpu,
      pids: Number(p.pids) || 0,
      slots: Number(p.slots) || 1,
    })),
  ]
  defaultsProblems.value = []
  showDefaultsDialog.value = true
}

function addDefaultProfileRow() {
  defaultProfileRows.value.push({ name: '', memory: '2g', cpu: '2', pids: 256, slots: 1 })
}

function removeDefaultProfileRow(index: number) {
  const row = defaultProfileRows.value[index]
  if (row?.name === 'default') return
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
    if (name !== 'default' && !PROFILE_NAME_RE.test(name)) {
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
  if (!seenNames.has('default')) {
    profileProblems.unshift("profiles must define the reserved 'default' profile")
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
      sandboxHostsApi.changes('pending'),
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

/** One-line "event" text for a stored config version, derived from the
 *  document itself (the history table keeps no description column):
 *  which tiers it offers, which profiles, and the network. */
function versionEvent(v: ConfigVersion): string {
  const doc = v.document
  if (!doc || typeof doc !== 'object') return 'empty document'
  const tierNames = Object.keys(doc.tiers ?? {})
  const profileNames = Object.keys(doc.profiles ?? {})
  const parts: string[] = []
  parts.push(tierNames.length ? `tiers: ${tierNames.join(', ')}` : 'no tiers')
  parts.push(profileNames.length ? `profiles: ${profileNames.join(', ')}` : 'no profiles')
  if (doc.network) parts.push(`network ${doc.network}`)
  return parts.join(' · ')
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
        <UiButton size="sm" @click="openEnroll"><Plus class="h-4 w-4" /> Enroll host</UiButton>
      </div>
    </div>

    <!-- ── Fleet policy: profile grants ── -->
    <div class="mb-3">
      <div class="flex items-center gap-3">
        <h2 class="text-sm font-semibold tracking-tight">Fleet policy</h2>
        <span class="h-px flex-1 bg-border/50"></span>
      </div>
      <p class="mt-0.5 text-[10.5px] text-muted-foreground">Team-level authorization applied across every sandbox host.</p>
    </div>
    <div class="rounded-xl border border-border/50 bg-card overflow-hidden">
      <div class="px-5 pt-3.5 pb-3">
        <h2 class="text-[13px] font-semibold tracking-tight">Profile grants</h2>
        <p class="mt-0.5 text-[10.5px] text-muted-foreground">
          Which teams may request each non-default resource profile, and where that profile is offered — fleet-wide.
          <code class="mono">default</code> is implicit for every team.
        </p>
      </div>
      <table class="w-full text-[12px]">
        <thead>
          <tr class="border-y border-border/40 bg-muted/10">
            <th class="px-5 py-2 text-left text-[9.5px] font-semibold uppercase tracking-[0.08em] text-muted-foreground">Profile</th>
            <th class="px-4 py-2 text-left text-[9.5px] font-semibold uppercase tracking-[0.08em] text-muted-foreground">Granted teams</th>
            <th class="px-4 py-2 text-left text-[9.5px] font-semibold uppercase tracking-[0.08em] text-muted-foreground">Offered by</th>
            <th class="w-10"></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="row in grantProfileRows" :key="row.profile" class="border-b border-border/30 last:border-b-0">
            <td class="px-5 py-2.5 align-top">
              <span class="font-mono text-[11.5px] font-medium text-foreground">{{ row.profile }}</span>
              <span
                class="mt-0.5 block text-[9.5px]"
                :class="row.teams.length ? 'text-muted-foreground' : 'text-amber-600'"
              >
                {{ row.teams.length
                  ? `offered by ${row.offeredBy.length || 'no'} host${row.offeredBy.length === 1 ? '' : 's'}`
                  : 'no grant — no team can request this profile' }}
              </span>
            </td>
            <td class="px-4 py-2.5 align-top">
              <div class="flex flex-wrap items-center gap-1.5">
                <span
                  v-for="t in row.teams"
                  :key="t.teamId"
                  class="inline-flex items-center gap-1 rounded-full bg-muted py-0.5 pl-2.5 pr-1 text-[11px] text-foreground/80"
                >
                  {{ t.teamName }}
                  <button
                    type="button"
                    class="inline-flex h-4 w-4 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-red-500/20 hover:text-red-500"
                    :title="`Revoke ${row.profile} from ${t.teamName}`"
                    @click="revokeGrant(t.grant)"
                  >
                    <X class="h-2.5 w-2.5" />
                  </button>
                </span>
                <button
                  v-if="grantPanelProfile !== row.profile"
                  type="button"
                  class="inline-flex items-center gap-1 rounded-full border border-dashed border-border/70 px-2 py-0.5 text-[10px] text-muted-foreground transition-colors hover:border-border-strong hover:text-foreground"
                  :title="grantableTeams(row.profile).length ? `Grant ${row.profile} to a team` : 'All teams already hold this grant'"
                  :disabled="!grantableTeams(row.profile).length"
                  @click="openGrantPanel(row.profile)"
                >
                  <Plus class="h-2.5 w-2.5" /> team
                </button>
                <div
                  v-else
                  class="flex items-center gap-1.5 rounded-full border border-border/60 bg-muted/40 px-2 py-0.5"
                >
                  <select
                    v-model="grantPanelTeamId"
                    class="max-w-[110px] border-0 bg-transparent text-[11px] text-foreground outline-hidden"
                    aria-label="Team"
                  >
                    <option value="" disabled>team…</option>
                    <option v-for="t in grantableTeams(row.profile)" :key="t.id" :value="t.id">{{ t.name }}</option>
                  </select>
                  <button
                    type="button"
                    class="inline-flex h-5 w-5 items-center justify-center rounded-full bg-primary text-primary-foreground transition-opacity"
                    :class="!grantPanelTeamId && 'opacity-40'"
                    :disabled="!grantPanelTeamId || grantSaving"
                    title="Add grant"
                    @click="addGrant(row.profile)"
                  >
                    <Check class="h-3 w-3" />
                  </button>
                  <button
                    type="button"
                    class="inline-flex h-5 w-5 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-muted"
                    title="Cancel"
                    @click="closeGrantPanel"
                  >
                    <X class="h-3 w-3" />
                  </button>
                </div>
              </div>
              <p v-if="grantError && grantPanelProfile === row.profile" class="mt-1 text-[10px] text-destructive">{{ grantError }}</p>
            </td>
            <td class="px-4 py-2.5 align-top">
              <div v-if="row.offeredBy.length" class="flex max-w-[220px] flex-wrap gap-1">
                <span
                  v-for="h in row.offeredBy"
                  :key="h"
                  class="inline-flex items-center rounded bg-emerald-500/10 px-1.5 py-0.5 font-mono text-[10px] leading-snug text-emerald-700 dark:text-emerald-400"
                >{{ h }}</span>
              </div>
              <span v-else class="font-mono text-[10px] italic leading-snug text-muted-foreground/60">no host offers it</span>
            </td>
            <td class="px-2 py-2.5 align-top">
              <UiButton
                v-if="row.teams.length"
                size="sm"
                variant="ghost"
                class="float-right"
                :title="`Revoke every grant for ${row.profile}`"
                @click="row.teams.forEach((t) => revokeGrant(t.grant))"
              ><Trash2 class="h-3.5 w-3.5" /></UiButton>
            </td>
          </tr>
          <tr class="border-t border-border/40 bg-muted/20">
            <td colspan="4" class="px-5 py-2.5">
              <div class="flex flex-wrap items-center gap-2">
                <select
                  v-model="newGrantTeamId"
                  class="rounded-md border border-border/60 bg-background px-2 py-1 text-[11px] text-foreground outline-hidden focus:border-primary/50"
                  aria-label="Team"
                >
                  <option value="" disabled>team…</option>
                  <option v-for="t in teams" :key="t.id" :value="t.id">{{ t.name }}</option>
                </select>
                <span class="text-[10px] text-muted-foreground">may request</span>
                <select
                  v-model="newGrantProfile"
                  class="w-36 rounded-md border border-border/60 bg-background px-2 py-1 font-mono text-[11px] text-foreground outline-hidden focus:border-primary/50"
                  aria-label="Profile name"
                  @change="grantError = ''"
                >
                  <option value="" disabled>profile…</option>
                  <option v-for="name in grantableProfileNames" :key="name" :value="name">{{ name }}</option>
                </select>
                <span
                  v-if="!grantableProfileNames.length"
                  class="text-[10px] italic text-muted-foreground/70"
                >no profiles offered yet — configure one on a host first</span>
                <UiButton
                  size="sm"
                  variant="outline"
                  :loading="grantSaving"
                  :disabled="!newGrantTeamId || !newGrantProfile.trim()"
                  @click="addNewProfileGrant"
                >
                  <Plus class="h-3.5 w-3.5" /> Grant
                </UiButton>
                <p v-if="grantError && !grantPanelProfile" class="text-[10px] text-destructive">{{ grantError }}</p>
              </div>
            </td>
          </tr>
        </tbody>
      </table>
      <div class="flex flex-wrap items-center gap-2 border-t border-border/40 px-5 py-2 text-[10px] text-muted-foreground/80">
        <span>Revoking stops the team's pipelines requesting that profile at trigger (4xx) and again at claim.</span>
        <span class="ml-auto"><code class="mono">default</code> is implicit for every team — not listed.</span>
      </div>
    </div>

    <!-- ── Machines: enrolled sandboxd instances ── -->
    <div class="mb-3 mt-6">
      <div class="flex items-center gap-3">
        <h2 class="text-sm font-semibold tracking-tight">Enrolled hosts</h2>
        <span class="h-px flex-1 bg-border/50"></span>
      </div>
      <p class="mt-0.5 text-[10.5px] text-muted-foreground">buz-sandboxd instances by host — each offers the platform catalog unless overridden.</p>
    </div>
    <div :class="changes.length ? 'grid grid-cols-1 items-start gap-4 xl:grid-cols-[3fr_1fr]' : ''">
      <div class="min-w-0 space-y-4">
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
                v-for="p in offeredProfilesFor(host)"
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

      <div v-if="changes.length" class="min-w-0 space-y-2">
        <div class="flex items-center gap-2">
          <h3 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground">Pending changes</h3>
          <span class="rounded-full bg-amber-500/15 px-1.5 text-xs text-amber-600">{{ changes.length }}</span>
        </div>
        <div v-for="change in changes" :key="change.id" class="rounded-xl border border-border/50 bg-card p-3 space-y-2">
          <div class="min-w-0">
            <p class="text-xs font-medium truncate">{{ change.host_id }}/{{ change.instance_id }}</p>
            <p class="type-meta truncate">requested by {{ change.requested_by }} · {{ change.created_at }}</p>
          </div>
          <pre class="text-[10px] bg-muted/40 rounded-lg p-2 overflow-auto max-h-32">{{ JSON.stringify(change.after_document, null, 2) }}</pre>
          <div class="flex justify-end gap-1.5">
            <UiButton size="sm" @click="decide(change, true)"><Check class="h-3 w-3" /> Approve</UiButton>
            <UiButton size="sm" variant="ghost" @click="decide(change, false)"><X class="h-3 w-3" /> Reject</UiButton>
          </div>
        </div>
      </div>
    </div>

    <!-- ── host detail dialog: config document ── -->
    <div v-if="detailHost" class="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4" @click.self="detailHost = null">
      <div class="bg-card rounded-xl border border-border/60 p-6 w-full max-w-3xl max-h-[85vh] overflow-y-auto space-y-5 shadow-lg">
        <div class="flex items-start justify-between gap-2">
          <div class="min-w-0">
            <h3 class="text-sm font-medium truncate">{{ detailHost.host_id }} / {{ detailHost.instance_id }}</h3>
            <p class="type-meta">
              budget {{ detailHost.max_resource_slots || '∞' }} slots · applied v{{ detailHost.applied_version }} of desired v{{ detailHost.desired_version }} (next = max + 1)<span v-if="detailHost.pushed && detailHost.applied_at"> · pushed {{ detailHost.applied_at }}</span>
            </p>
          </div>
          <div class="flex items-center gap-2 shrink-0">
            <span
              class="inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium uppercase tracking-wide"
              :class="detailHost.pushed ? 'bg-emerald-500/12 text-emerald-600 dark:text-emerald-400' : 'bg-amber-500/12 text-amber-600'"
              :title="detailHost.pushed
                ? `Applied version ${detailHost.applied_version}${detailHost.applied_at ? ` on ${detailHost.applied_at}` : ''}`
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

        <!-- 1. config document — Job sizes cards + isolation switches -->
        <section class="space-y-5">
          <div class="flex items-center justify-between gap-2">
            <h4 class="text-xs font-semibold uppercase tracking-wide text-muted-foreground">Config document</h4>
            <button
              type="button"
              class="text-xs text-muted-foreground transition-colors hover:text-foreground"
              @click="toggleRawJson"
            >
              {{ showRawJson ? '← Use form' : 'Raw JSON' }}
            </button>
          </div>

          <template v-if="!showRawJson">
            <!-- Job sizes: a selection surface over the platform catalog -->
            <div class="space-y-2">
              <h4 class="text-sm font-semibold">Job sizes <span class="text-xs font-normal text-muted-foreground">(resource profiles)</span></h4>
              <p class="type-meta">
                The catalog lives in <button type="button" class="text-primary underline underline-offset-2" @click="openDefaultsDialog">Platform defaults</button> — every entry is offered here unless you drop its chip; a dropped chip is stored as an exclusion on this machine. Names and numbers are edited only in the catalog.
              </p>
              <div class="flex flex-wrap gap-2.5">
                <!-- catalog profiles: offered unless excluded -->
                <div
                  v-for="name in catalogSizes"
                  :key="name"
                  class="relative w-44 cursor-pointer rounded-xl border px-3 py-2.5 transition-colors"
                  :class="excludedSizes.has(name)
                    ? 'border-dashed border-border/70 opacity-75 hover:opacity-100'
                    : sizeGrantLabel(name).granted ? 'border-border/60 hover:border-border' : 'border-red-500/35'"
                  role="button"
                  tabindex="0"
                  :aria-pressed="!excludedSizes.has(name)"
                  :title="excludedSizes.has(name)
                    ? `Excluded on this host — click to offer '${name}' from the catalog`
                    : `Offering '${name}' — click to exclude it from this host`"
                  @click="toggleCatalogSize(name)"
                  @keydown.enter.prevent="toggleCatalogSize(name)"
                >
                  <span class="flex items-center gap-1.5 text-[12px] font-semibold">
                    {{ name }}
                    <span
                      class="rounded px-1 py-px text-[8.5px] font-medium"
                      :class="excludedSizes.has(name)
                        ? 'bg-muted text-muted-foreground'
                        : sizeGrantLabel(name).granted ? 'bg-emerald-500/12 text-emerald-600' : 'bg-red-500/12 text-red-600'"
                    >{{ excludedSizes.has(name) ? 'excluded' : sizeGrantLabel(name).text }}</span>
                  </span>
                  <span class="mt-0.5 block font-mono text-[9.5px] text-muted-foreground">{{ specLine(catalogSizeRow(name)) }}</span>
                  <span class="mt-1 block text-[9.5px] text-muted-foreground/70">{{ excludedSizes.has(name) ? 'click to offer' : sizeGrantLabel(name).granted ? `grant: ${sizeGrantLabel(name).text}` : 'no team may request it yet' }}</span>
                </div>
                <!-- legacy rows the host document still defines itself -->
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
              </div>
              <p v-if="fallbackSize" class="text-[10px] leading-relaxed text-muted-foreground">
                Fallback envelope <code class="mono">default</code> — <span class="font-mono">{{ specLine(fallbackSize) }}</span> — runs implicitly for every job without <code class="mono">runner.resources</code>; every team may use it. Its numbers are edited in <button type="button" class="text-primary underline underline-offset-2" @click="openDefaultsDialog">Platform defaults</button>.
              </p>
              <p class="text-[10px] leading-relaxed text-muted-foreground">
                “no grant” = the profile exists here but no team may request it — grant it under <strong>Profile grants</strong> above. Disk is a fixed tmpfs and GPU passthrough is not yet supported — those follow platform evolution.
              </p>
            </div>

          </template>

          <template v-else>
            <p class="type-meta">Paste a document and apply it to the form — it is validated there before you can publish.</p>
            <textarea
              v-model="rawJsonText"
              rows="14"
              spellcheck="false"
              class="w-full font-mono text-[11px] rounded-lg border border-border/60 bg-background p-2"
            ></textarea>
            <UiButton size="sm" variant="ghost" @click="applyRawJson"><Check class="h-3.5 w-3.5" /> Apply to form</UiButton>
          </template>

          <ul v-if="configProblems.length" class="text-xs text-destructive list-disc list-inside space-y-0.5">
            <li v-for="p in configProblems" :key="p">{{ p }}</li>
          </ul>
          <ul v-if="configWarnings.length" class="text-xs text-amber-600 list-disc list-inside">
            <li v-for="w in configWarnings" :key="w">{{ w }}</li>
          </ul>
          <p v-if="configValid && !configProblems.length" class="flex items-start gap-1 text-xs text-emerald-600">
            <Check class="h-3.5 w-3.5 mt-0.5 shrink-0" />
            <span>
              Checks passed — profiles, tier images (digest-pinned) and network are accepted. Publish stages this as the next desired version; sandboxd picks it up on its next fetch.
            </span>
          </p>
          <div class="flex gap-2">
            <UiButton size="sm" variant="ghost" :disabled="savingConfig" @click="validateConfig">
              <ShieldCheck class="h-3.5 w-3.5" /> Validate
            </UiButton>
            <UiButton size="sm" :loading="savingConfig" @click="saveConfig">Publish</UiButton>
          </div>

          <div v-if="versionsLoading || versions.length" class="pt-1">
            <h4 class="text-xs font-medium text-muted-foreground mb-1">Version history</h4>
            <div v-if="versionsLoading" class="type-meta">Loading…</div>
            <ul v-else class="divide-y divide-border/60 text-xs">
              <li
                v-for="v in versions"
                :key="v.version"
                class="grid grid-cols-[2.5rem_minmax(0,1fr)_6.25rem_10rem_4.5rem] items-baseline gap-x-2 py-1.5"
              >
                <span class="font-mono text-[11px] text-muted-foreground">v{{ v.version }}</span>
                <span class="min-w-0 break-words">{{ versionEvent(v) }}</span>
                <span class="type-meta whitespace-nowrap">{{ formatDateTime(v.created_at) }}</span>
                <span class="min-w-0 break-words type-meta">{{ v.created_by }}</span>
                <span v-if="v.version === detailHost.desired_version" class="justify-self-end rounded bg-muted px-1 py-px text-[9px] text-muted-foreground">current</span>
                <UiButton v-else size="sm" variant="ghost" class="justify-self-end" @click="rollbackTo(v.version)">Restore</UiButton>
              </li>
            </ul>
          </div>
        </section>

        
      </div>
    </div>

    <!-- ── platform default tiers dialog ── -->
    <div v-if="showDefaultsDialog" class="fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4" @click.self="showDefaultsDialog = false">
      <div class="bg-card rounded-xl border border-border/60 p-6 w-full max-w-2xl max-h-[85vh] overflow-y-auto space-y-4 shadow-lg">
        <div class="flex items-start justify-between gap-2">
          <div class="min-w-0">
            <h3 class="text-sm font-medium">Platform defaults</h3>
            <p class="type-meta">
              Values every host inherits unless its own document overrides them. Saving bumps the desired version of each host whose effective document changes, so hosts re-fetch.
            </p>
          </div>
          <UiButton size="sm" variant="ghost" @click="showDefaultsDialog = false"><X class="h-4 w-4" /></UiButton>
        </div>

        <!-- ── Section 1: Isolation levels ── -->
        <div class="space-y-1">
          <h4 class="text-sm font-semibold">Isolation levels</h4>
          <p class="type-meta">
            The envelope image each isolation level runs in. Leave a row empty to provide no default for that level (jobs selecting it fail closed until an image exists).
          </p>
        </div>
        <div class="space-y-2">
          <div
            v-for="(row, i) in defaultsRows"
            :key="row.name"
            class="grid grid-cols-2 gap-2 rounded-lg border border-border/40 p-2.5 sm:grid-cols-7 sm:items-end"
          >
            <span class="block col-span-1">
              <span class="mb-0.5 block text-[10px] font-medium uppercase tracking-wide text-muted-foreground">Tier</span>
              <span class="block rounded-md border border-border/40 bg-muted/30 px-2 py-1 font-mono text-xs">{{ row.name }}</span>
            </span>
            <div class="block col-span-2 sm:col-span-4">
              <span class="mb-0.5 block text-[10px] font-medium uppercase tracking-wide text-muted-foreground">Image — tag or digest</span>
              <div class="flex gap-1">
                <input
                  v-model="row.image"
                  :placeholder="TIER_IMAGE_HINTS[row.name] ?? `no default for ${row.name}`"
                  class="min-w-0 flex-1 rounded-md border border-border/60 bg-background px-2 py-1 font-mono text-xs outline-hidden focus:border-primary/50"
                />
                <UiButton
                  size="sm"
                  variant="ghost"
                  :loading="resolvingTier === row.image.trim()"
                  :disabled="!row.image.trim() || isPinnedImage(row.image) || resolvingTier !== null"
                  title="Resolve this tag to a digest now (Save also does this)"
                  @click="resolveDefaultsRow(row)"
                ><RefreshCw class="h-3.5 w-3.5" /></UiButton>
              </div>
              <span v-if="isPinnedImage(row.image)" class="mt-0.5 flex items-center gap-1 text-[10px] text-emerald-600">
                <Check class="h-3 w-3" /> digest pinned
              </span>
            </div>
            <label class="block col-span-2 sm:col-span-2">
              <span class="mb-0.5 block text-[10px] font-medium uppercase tracking-wide text-muted-foreground">Runtime</span>
              <input
                v-model="row.runtime"
                list="oci-runtime-options"
                placeholder="default (runc)"
                class="w-full rounded-md border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50"
              />
              <span class="mt-0.5 block text-[9.5px] text-muted-foreground">Empty = daemon default (runc)</span>
            </label>
            <label class="block max-w-56">
              <span class="mb-0.5 block text-[10px] font-medium uppercase tracking-wide text-muted-foreground">Docker network</span>
              <input
                v-model="platformDefaultNetwork"
                placeholder="bridge"
                class="w-full rounded-md border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50"
              />
              <span class="mt-0.5 block text-[9.5px] text-muted-foreground">Inherited by hosts whose document leaves it unset.</span>
            </label>
          </div>
        </div>

        <!-- ── Section 2: Job sizes ── -->
        <div class="space-y-1 border-t border-border/40 pt-3">
          <h4 class="text-sm font-semibold">Job sizes</h4>
          <p class="type-meta">
            The resource-profile catalog — the single place where names and numbers are defined. Every host offers all of it unless the host's page excludes entries it cannot serve; a name means the same envelope on every machine. Pipelines request one via <code class="mono-chip text-[10px]">runner.resources: &lt;name&gt;</code>; <code class="mono-chip text-[10px]">default</code> is required and applies to jobs without <code class="mono-chip text-[10px]">resources</code>.
          </p>
        </div>
        <div class="space-y-1.5">
          <div class="space-y-1.5">
            <div
              v-for="(row, i) in defaultProfileRows"
              :key="i"
              class="grid grid-cols-2 gap-2 rounded-lg border p-2.5 sm:grid-cols-6 sm:items-end"
              :class="row.name === 'default' ? 'border-primary/30 bg-primary/5' : 'border-border/40'"
            >
              <label class="block col-span-2 sm:col-span-1">
                <span class="mb-0.5 block text-[10px] font-medium uppercase tracking-wide text-muted-foreground">Name</span>
                <input
                  v-model="row.name"
                  :disabled="row.name === 'default'"
                  placeholder="large"
                  class="w-full rounded-md border border-border/60 bg-background px-2 py-1 font-mono text-xs outline-hidden focus:border-primary/50 disabled:opacity-70"
                />
              </label>
              <label class="block">
                <span class="mb-0.5 block text-[10px] font-medium uppercase tracking-wide text-muted-foreground">Memory</span>
                <input v-model="row.memory" placeholder="2g" class="w-full rounded-md border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50" />
              </label>
              <label class="block">
                <span class="mb-0.5 block text-[10px] font-medium uppercase tracking-wide text-muted-foreground">CPU</span>
                <input v-model="row.cpu" placeholder="2" class="w-full rounded-md border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50" />
              </label>
              <label class="block">
                <span class="mb-0.5 block text-[10px] font-medium uppercase tracking-wide text-muted-foreground">Pids</span>
                <input v-model.number="row.pids" type="number" min="1" class="w-full rounded-md border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50" />
              </label>
              <label class="block">
                <span class="mb-0.5 block text-[10px] font-medium uppercase tracking-wide text-muted-foreground">Slots</span>
                <input v-model.number="row.slots" type="number" min="1" max="32" class="w-full rounded-md border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50" />
              </label>
              <div class="flex items-end col-span-2 sm:col-span-1">
                <UiButton
                  size="sm"
                  variant="ghost"
                  :disabled="row.name === 'default'"
                  :title="row.name === 'default' ? 'The default profile cannot be removed' : `Remove ${row.name || '(unnamed)'}`"
                  @click="removeDefaultProfileRow(i)"
                ><Trash2 class="h-3.5 w-3.5" /></UiButton>
              </div>
            </div>
            <UiButton size="sm" variant="ghost" @click="addDefaultProfileRow"><Plus class="h-3.5 w-3.5" /> Add job size</UiButton>
          </div>

          <label class="block max-w-56">
            <span class="mb-0.5 block text-[10px] font-medium uppercase tracking-wide text-muted-foreground">Host slot budget</span>
            <input
              v-model.number="platformDefaultSlots"
              type="number"
              min="0"
              class="w-full rounded-md border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50"
              title="Max slots across every runner on a host that hasn't set its own; 0 = unlimited"
            />
            <span class="mt-0.5 block text-[9.5px] text-muted-foreground">Fleet-wide cap for hosts without their own budget; 0 = unlimited.</span>
          </label>
        </div>

        <ul v-if="defaultsProblems.length" class="text-xs text-destructive list-disc list-inside space-y-0.5">
          <li v-for="p in defaultsProblems" :key="p">{{ p }}</li>
        </ul>

        <datalist id="oci-runtime-options">
          <option value="runc">runc — standard OCI runtime</option>
          <option value="crun">crun — lightweight OCI runtime</option>
          <option value="runsc">runsc — gVisor userspace kernel</option>
          <option value="kata-fc">kata-fc — Kata Firecracker microVM</option>
          <option value="kata-qemu">kata-qemu — Kata QEMU microVM</option>
          <option value="sysbox-runc">sysbox-runc — Sysbox container sandbox</option>
        </datalist>

        <div class="flex justify-end gap-2">
          <UiButton size="sm" variant="ghost" @click="showDefaultsDialog = false">Cancel</UiButton>
          <UiButton size="sm" :loading="savingDefaults" @click="saveDefaults">Save defaults</UiButton>
        </div>
      </div>
    </div>

    <!-- ── enroll / token dialog ── -->
    <div v-if="showEnroll" class="fixed inset-0 z-50 flex items-center justify-center bg-black/40" @click.self="showEnroll = false">
      <div class="bg-card rounded-xl border border-border/60 p-6 w-full max-w-md space-y-4 shadow-lg">
        <template v-if="!issuedToken">
          <h3 class="text-sm font-medium">Enroll sandbox host</h3>
          <label class="block">
            <span class="mb-1 block text-xs font-medium">Host</span>
            <input v-model="enrollHostId" list="known-host-ids" placeholder="web-1" class="w-full text-sm rounded-lg border border-border/60 bg-background px-3 py-2" @input="enrollError = ''" />
          </label>
          <p v-if="enrollHostId.trim() && !knownHostIds.includes(enrollHostId.trim())" class="type-meta">
            New host <span class="font-mono">{{ enrollHostId.trim() }}</span> — instance ID is generated automatically.
          </p>
          <p v-else-if="instancesOnHost.length" class="type-meta">
            Existing host — {{ instancesOnHost.length }} instance(s) enrolled.
          </p>
          <datalist id="known-host-ids">
            <option v-for="h in knownHostIds" :key="h" :value="h" />
          </datalist>
          <button type="button" class="inline-flex items-center gap-1 self-start text-xs text-muted-foreground transition-colors hover:text-foreground" @click="showAdvanced = !showAdvanced">
            <ChevronRight class="h-3.5 w-3.5 transition-transform" :class="showAdvanced && 'rotate-90'" />
            Advanced
          </button>
          <div v-if="showAdvanced" class="space-y-3 border-l border-border/40 pl-3">
            <label class="block">
              <span class="mb-1 block text-xs font-medium">Instance ID</span>
              <input v-model="enrollInstanceId" placeholder="sandboxd-web-1" class="w-full text-sm rounded-lg border border-border/60 bg-background px-3 py-2" @input="instanceEdited = true; enrollError = ''" />
            </label>
            <p class="type-meta">Instance ID and host slot budget can also be adjusted later in the host's config sheet.</p>
          </div>
          <p v-if="enrollError" class="text-xs text-red-500">{{ enrollError }}</p>
          <div class="flex justify-end gap-2">
            <UiButton size="sm" variant="ghost" @click="showEnroll = false">Cancel</UiButton>
            <UiButton size="sm" :loading="creating" :disabled="!enrollHostId.trim() || !enrollInstanceId.trim()" @click="handleEnroll">Enroll</UiButton>
          </div>
        </template>
        <template v-else>
          <h3 class="text-sm font-medium flex items-center gap-2"><KeyRound class="h-4 w-4" /> Bootstrap env (shown once)</h3>
          <p class="type-meta">
            Paste on the sandboxd machine — shown once, only the token's SHA-256 is stored. Prefer a token file (mode 0400, <code class="mono-chip text-[10px]">BUZ_SANDBOXD_TOKEN_FILE</code>) when possible.
          </p>
          <pre class="max-h-48 overflow-auto whitespace-pre-wrap rounded-lg bg-muted/40 p-3 text-xs">{{ bootstrapSnippet }}</pre>
          <div class="flex justify-end gap-2">
            <UiButton size="sm" variant="ghost" @click="copySnippet">
              <Copy class="h-3.5 w-3.5" /> {{ snippetCopied ? 'Copied' : 'Copy env' }}
            </UiButton>
            <UiButton size="sm" variant="ghost" @click="copyToken">
              <Copy class="h-3.5 w-3.5" /> {{ tokenCopied ? 'Copied' : 'Token only' }}
            </UiButton>
            <UiButton size="sm" @click="showEnroll = false">Done</UiButton>
          </div>
        </template>
      </div>
    </div>
  </div>
</template>
