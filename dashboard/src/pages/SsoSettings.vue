<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import {
  AlertTriangle,
  BookOpen,
  Check,
  Clock,
  Copy,
  Database,
  Link2,
  Plug,
  RotateCcw,
  ShieldCheck,
} from '@lucide/vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import TablePagination from '@/components/ui/TablePagination.vue'
import UiButton from '@/components/ui/UiButton.vue'
import { useAppDialog } from '@/composables/useAppDialog'
import {
  oidcSettingsApi,
  type DiscoveryProbe,
  type OidcEvent,
  type OidcSettings,
  type TokenAuthMethod,
} from '@/api/oidcSettings'
import { oidcIdentitiesApi } from '@/api/oidcIdentities'
import { sessionPolicyApi, type SessionPolicy } from '@/api/sessionPolicy'

// SSO settings, database-only configuration (see platform/oidc-sso.md).
// Composition per the approved v4 design: a status strip, then a two-column
// grid — the provider connection (and, once configured, the registration
// card) in the main column, the sign-in policy (first-time: the registration
// card) in the 320px sidebar. The client secret is write-only — the API
// never echoes it back; a first save requires it.
const DOCS_URL = 'https://buz.docs.sconts.com/docs/01-architecture/platform/oidc-sso/'

const router = useRouter()
const dialog = useAppDialog()

const loading = ref(true)
const saving = ref(false)
const testing = ref(false)

const enabled = ref(false)
const issuer = ref('')
const clientId = ref('')
const clientSecret = ref('') // write-only: empty = keep the stored one
const redirectOverride = ref('')
const scopesInput = ref('openid, profile, email')
const autoProvision = ref(true)
const tokenAuth = ref<TokenAuthMethod>('auto')
const groupsClaim = ref('groups')

// Access policy (global: local and SSO sign-ins alike). The idle value is
// enforced by the SPA only — the copy must not imply server-side revocation.
const policy = ref<SessionPolicy | null>(null)
const policyTtl = ref(24)
const policyIdle = ref(0)
const savingAccess = ref(false)
const accessNotice = ref('')
const accessError = ref('')

// Recent SSO events (platform-wide for platform admins; no local logins).
const events = ref<OidcEvent[]>([])
const eventsTotal = ref(0)
const eventsPage = ref(1)
const eventsPageSize = ref(20)
const eventsLoading = ref(false)
const eventsError = ref('')

const eventsPageCount = computed(() =>
  Math.max(1, Math.ceil(eventsTotal.value / eventsPageSize.value) || 1),
)

const eventsRangeLabel = computed(() => {
  if (eventsTotal.value === 0) return '0 of 0'
  const start = (eventsPage.value - 1) * eventsPageSize.value + 1
  const end = Math.min(eventsPage.value * eventsPageSize.value, eventsTotal.value)
  return `${start}–${end} of ${eventsTotal.value}`
})

const savedOnce = ref(false)
const active = ref(false)
const updatedBy = ref('')
const validationError = ref('')
const probe = ref<DiscoveryProbe | null>(null)
const lastProbeAt = ref<number | null>(null)
const replacingSecret = ref(false)
const pendingCount = ref(0)
const savingPolicy = ref(false)
const policyNotice = ref('')
const policyError = ref('')
let policyNoticeTimer: number | undefined
const copiedKey = ref('')
const notice = ref('')
const error = ref('')

const tick = ref(0)
const timer = window.setInterval(() => {
  tick.value += 1
}, 30_000)
onUnmounted(() => window.clearInterval(timer))

const oidcMeta = ref<OidcSettings | null>(null)

const firstTime = computed(() => !savedOnce.value)

// A probe only proves the discovery document is reachable. `issuer_match` is
// the check the runtime itself enforces — when it is false, SSO stays inactive
// even though "discovery OK", because the provider is advertising a different
// issuer (e.g. `localhost` vs `127.0.0.1`).
const probeHealthy = computed(
  () => probe.value?.ok === true && probe.value.issuer_match !== false,
)
// Enabled but no runtime: the configuration was rejected (or the provider is
// down at save/startup time). The UI must say so instead of leaving a bare
// "Inactive" next to a switched-on toggle.
const runtimeInactive = computed(() => enabled.value && !active.value)

const probeAge = computed(() => {
  if (lastProbeAt.value === null) return ''
  void tick.value // re-render on the interval
  const minutes = Math.max(1, Math.floor((Date.now() - lastProbeAt.value) / 60_000))
  return `${minutes}m ago`
})

const scopes = computed(() =>
  scopesInput.value
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean),
)

const registrationUris = computed(() => [
  {
    key: 'callback',
    label: 'Callback',
    what: 'sign-in',
    // Empty until the first save; `derived_redirect_uri` is what the Manager
    // would store, so the operator can register it before saving.
    value: oidcMeta.value?.redirect_uri_effective || oidcMeta.value?.derived_redirect_uri || '',
  },
  // Both are derived from server.public_base_url server-side, so they are
  // present even with no saved configuration.
  { key: 'backchannel', label: 'Back-channel logout', what: 'instant revocation', value: oidcMeta.value?.backchannel_logout_uri ?? '' },
  { key: 'landing', label: 'Post-logout landing', what: 'unified sign-out', value: oidcMeta.value?.post_logout_redirect_uri ?? '' },
])

function apply(settings: OidcSettings) {
  oidcMeta.value = settings
  savedOnce.value = settings.updated_by !== ''
  enabled.value = settings.enabled
  issuer.value = settings.issuer
  clientId.value = settings.client_id
  clientSecret.value = ''
  replacingSecret.value = false
  redirectOverride.value = settings.redirect_uri_override ?? ''
  scopesInput.value = settings.scopes.join(', ')
  autoProvision.value = settings.auto_provision
  tokenAuth.value = settings.token_auth ?? 'auto'
  groupsClaim.value = settings.groups_claim ?? 'groups'
  active.value = settings.active
  updatedBy.value = settings.updated_by
  validationError.value =
    settings.validation.ok === false ? (settings.validation.error ?? '') : ''
}

async function loadPendingCount() {
  try {
    pendingCount.value = (await oidcIdentitiesApi.listPending()).length
  } catch {
    pendingCount.value = 0
  }
}

onMounted(async () => {
  try {
    apply(await oidcSettingsApi.get())
    await loadPendingCount()
  } catch (e: any) {
    error.value = e.message || 'Failed to load SSO settings'
  } finally {
    loading.value = false
  }
  await loadPolicy()
  await loadEvents()
})

async function loadPolicy() {
  try {
    policy.value = await sessionPolicyApi.get()
    policyTtl.value = policy.value.ttl_hours
    policyIdle.value = policy.value.idle_minutes
    accessError.value = ''
  } catch (e: any) {
    accessError.value = e.message || 'Failed to load the access policy'
  }
}

async function savePolicy() {
  savingAccess.value = true
  accessError.value = ''
  accessNotice.value = ''
  try {
    policy.value = await sessionPolicyApi.update({
      ttl_hours: policyTtl.value,
      idle_minutes: policyIdle.value,
    })
    policyTtl.value = policy.value.ttl_hours
    policyIdle.value = policy.value.idle_minutes
    accessNotice.value = 'Access policy saved.'
    window.setTimeout(() => {
      accessNotice.value = ''
    }, 2500)
  } catch (e: any) {
    accessError.value = e.message || 'Saving the access policy failed'
  } finally {
    savingAccess.value = false
  }
}

async function resetPolicy() {
  savingAccess.value = true
  accessError.value = ''
  accessNotice.value = ''
  try {
    policy.value = await sessionPolicyApi.update({ ttl_hours: null, idle_minutes: null })
    policyTtl.value = policy.value.ttl_hours
    policyIdle.value = policy.value.idle_minutes
    accessNotice.value = 'Reset to the defaults.'
  } catch (e: any) {
    accessError.value = e.message || 'Reset failed'
  } finally {
    savingAccess.value = false
  }
}

async function loadEvents() {
  eventsLoading.value = true
  eventsError.value = ''
  try {
    const result = await oidcSettingsApi.events(eventsPage.value, eventsPageSize.value)
    events.value = result.items
    eventsTotal.value = result.total
    if (eventsPage.value > eventsPageCount.value) {
      eventsPage.value = eventsPageCount.value
    }
  } catch (e: any) {
    eventsError.value = e.message || 'Failed to load SSO events'
  } finally {
    eventsLoading.value = false
  }
}

watch([eventsPage, eventsPageSize], () => {
  void loadEvents()
})

const EVENT_LABELS: Record<string, string> = {
  'auth.oidc_login': 'Signed in',
  'auth.oidc_provision': 'Account created',
  'auth.oidc_identity_approve': 'Approved',
  'auth.oidc_identity_reject': 'Rejected',
  'auth.oidc_backchannel_logout': 'Sessions revoked',
}

const EVENT_CHIPS: Record<string, string> = {
  'auth.oidc_login': 'chip-success',
  'auth.oidc_provision': 'chip-success',
  'auth.oidc_identity_approve': 'chip-success',
  'auth.oidc_identity_reject': 'chip-danger',
  'auth.oidc_backchannel_logout': 'chip-warning',
}

function eventLabel(action: string): string {
  return EVENT_LABELS[action] ?? action
}

function eventChip(action: string): string {
  return EVENT_CHIPS[action] ?? 'chip-neutral'
}

function asText(value: unknown): string {
  return typeof value === 'string' ? value : ''
}

/**
 * The identity the event is about — email first, like the host project's
 * User column. For approve/reject this is the identity in the queue, not the
 * administrator who acted on it.
 */
function eventUser(event: OidcEvent): string {
  const after = event.after as Record<string, unknown> | null
  return (
    asText(after?.email) || asText(after?.username) || event.resource_name || '—'
  )
}

/**
 * One human sentence per event, written by the backend at audit time (the
 * same posture as the host project's `detail` field). The UI never composes
 * event text — and never shows the opaque IdP subject (Signet uses a UUID).
 */
function eventDetail(event: OidcEvent): string {
  const after = event.after as Record<string, unknown> | null
  // `reason` is the older backend-written sentence for back-channel rows
  // recorded before `detail` existed — still backend text, never composed here.
  return asText(after?.detail) || asText(after?.reason) || '—'
}

function formatEventTime(raw: string): string {
  if (!raw) return '—'
  // Audit timestamps are UTC text; show them in the viewer's local time.
  const parsed = new Date(raw.includes('T') ? raw : `${raw.replace(' ', 'T')}Z`)
  return Number.isNaN(parsed.getTime()) ? raw.slice(0, 19).replace('T', ' ') : parsed.toLocaleString()
}

async function testConnection() {
  testing.value = true
  probe.value = null
  try {
    probe.value = await oidcSettingsApi.test(issuer.value.trim())
    lastProbeAt.value = Date.now()
  } catch (e: any) {
    probe.value = { ok: false, error: e.message || 'Probe failed' }
    lastProbeAt.value = null
  } finally {
    testing.value = false
  }
}

async function save() {
  saving.value = true
  error.value = ''
  notice.value = ''
  try {
    const secretNeeded = replacingSecret.value || !oidcMeta.value?.client_secret_configured
    const settings = await oidcSettingsApi.update({
      enabled: enabled.value,
      issuer: issuer.value.trim(),
      client_id: clientId.value.trim(),
      client_secret: secretNeeded ? clientSecret.value.trim() || undefined : undefined,
      redirect_uri_override: redirectOverride.value.trim() || null,
      scopes: scopes.value,
      auto_provision: autoProvision.value,
      token_auth: tokenAuth.value,
      groups_claim: groupsClaim.value.trim() || 'groups',
    })
    apply(settings)
    probe.value = null
    lastProbeAt.value = null
    if (!settings.auto_provision) await loadPendingCount()
    notice.value = 'Saved and applied.'
  } catch (e: any) {
    error.value = e.message || 'Save failed'
  } finally {
    saving.value = false
  }
}

async function clearConfiguration() {
  if (
    !(await dialog.confirm(
      'Clear the SSO configuration? Sign-in through the provider turns off until settings are saved again. Local login is not affected.',
      { title: 'Clear configuration', confirmLabel: 'Clear', danger: true },
    ))
  )
    return
  error.value = ''
  notice.value = ''
  try {
    apply(await oidcSettingsApi.reset())
    probe.value = null
    lastProbeAt.value = null
    notice.value = 'SSO configuration cleared.'
  } catch (e: any) {
    error.value = e.message || 'Clear failed'
  }
}

async function copyUri(key: string, value: string) {
  try {
    await navigator.clipboard.writeText(value)
    copiedKey.value = key
    window.setTimeout(() => {
      if (copiedKey.value === key) copiedKey.value = ''
    }, 1500)
  } catch {
    // clipboard unavailable — the value stays selectable on screen
  }
}

/// Clicking a policy option saves it immediately — a targeted update built
/// from the **last-saved** values, so unsaved edits in the connection form
/// are never carried along. Failure reverts the selection; success shows a
/// brief confirmation inside the card.
async function setPolicy(value: boolean) {
  // Picking a policy saves immediately, so it only works once there is a saved
  // configuration to patch: before the first save there is no issuer or client
  // to send, and the PUT would be rejected. The radio is a preview then — the
  // value rides along with the next Save.
  if (firstTime.value) {
    autoProvision.value = value
    return
  }
  if (savingPolicy.value || value === autoProvision.value) return
  const previous = autoProvision.value
  autoProvision.value = value
  savingPolicy.value = true
  policyError.value = ''
  policyNotice.value = ''
  window.clearTimeout(policyNoticeTimer)
  try {
    const meta = oidcMeta.value
    const settings = await oidcSettingsApi.update({
      enabled: meta?.enabled ?? enabled.value,
      issuer: meta?.issuer ?? issuer.value,
      client_id: meta?.client_id ?? clientId.value,
      // omit the secret — the backend keeps the stored one
      redirect_uri_override: meta?.redirect_uri_override ?? null,
      scopes: meta?.scopes ?? scopes.value,
      auto_provision: value,
      // Carry the last-saved values so this targeted save never resets them.
      token_auth: meta?.token_auth ?? tokenAuth.value,
      groups_claim: meta?.groups_claim ?? groupsClaim.value,
    })
    autoProvision.value = settings.auto_provision
    policyNotice.value = 'Policy updated — applies immediately.'
    policyNoticeTimer = window.setTimeout(() => {
      policyNotice.value = ''
    }, 2500)
    if (!value) await loadPendingCount()
  } catch (e: any) {
    autoProvision.value = previous // snap back to what the server still has
    policyError.value = e?.message || 'Saving the policy failed'
  } finally {
    savingPolicy.value = false
  }
}
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      title="Single sign-on (OIDC)"
      description="Connect any standard OIDC provider. End-to-end verified with Signet."
    >
      <template #actions>
        <a
          class="inline-flex items-center gap-1.5 rounded-xl border border-border px-3 py-1.5 text-xs text-muted-foreground transition-colors hover:bg-muted/40 hover:text-foreground"
          :href="DOCS_URL"
          target="_blank"
          rel="noopener"
        >
          <BookOpen class="h-3.5 w-3.5" />
          Docs
        </a>
      </template>
    </PageHeader>

    <div
      v-if="loading"
      class="surface-card p-6 text-sm text-muted-foreground"
    >
      Loading…
    </div>

    <template v-else>
      <!-- status strip: one layout whether or not a configuration is saved -->
      <div
        class="flex flex-wrap items-center gap-x-3 gap-y-2 surface-card px-5 py-4"
      >
        <span
          class="inline-flex items-center gap-1.5"
          :class="active ? 'chip-success' : 'chip-neutral'"
        >
          <span class="h-1.5 w-1.5 rounded-full" :class="active ? 'bg-success' : 'bg-muted-foreground'" />
          {{ active ? 'Active' : 'Inactive' }}
        </span>
        <label class="inline-flex items-center gap-2 text-sm font-medium">
          <span class="relative inline-flex h-5 w-[34px]">
            <input v-model="enabled" type="checkbox" class="peer h-full w-full cursor-pointer appearance-none rounded-lg bg-muted-foreground/30 transition-colors checked:bg-success" />
            <span class="pointer-events-none absolute left-0.5 top-0.5 h-4 w-4 rounded-full bg-white transition-all peer-checked:translate-x-[14px]"></span>
          </span>
          Sign-in enabled
        </label>
        <span
          v-if="probeHealthy && lastProbeAt"
          class="chip-success gap-1.5"
        >
          <Check class="h-3 w-3" />
          Connection OK · {{ probeAge }}
        </span>
        <span
          v-else-if="probe?.ok && lastProbeAt"
          class="chip-warning gap-1.5"
          :title="`The provider reports ${probe.discovered_issuer}`"
        >
          <AlertTriangle class="h-3 w-3" />
          Issuer mismatch · {{ probeAge }}
        </span>
        <span v-if="!firstTime" class="chip-neutral gap-1.5">
          <Database class="h-3 w-3" />
          Source: database
        </span>
        <span v-else class="chip-neutral gap-1.5">
          <Database class="h-3 w-3" />
          No configuration yet
        </span>
        <span v-if="updatedBy" class="text-xs text-muted-foreground">Saved by {{ updatedBy }}</span>
        <UiButton
          v-if="!firstTime"
          variant="ghost"
          size="sm"
          class="ml-auto"
          @click="clearConfiguration"
        >
          <RotateCcw class="h-3.5 w-3.5" />
          Clear configuration
        </UiButton>
      </div>

      <p v-if="runtimeInactive" class="notice-warning text-sm">
        Sign-in is switched on, but the runtime is inactive. The provider's
        discovery document must report the issuer above character-for-character
        (<span class="font-mono">{{ issuer }}</span>) — run Test connection to
        see what it actually reports.
      </p>

      <p v-if="!loading && error" class="field-error">{{ error }}</p>

      <!-- two-column composition: main column + 320px sidebar -->
      <div class="grid items-start gap-6 lg:grid-cols-[minmax(0,1fr)_320px]">
        <!-- main column -->
        <div class="flex min-w-0 flex-col gap-6">
          <!-- provider connection -->
          <div class="surface-card">
            <div class="flex items-center gap-2.5 px-5 pt-4">
              <span class="flex h-[30px] w-[30px] flex-none items-center justify-center rounded-lg bg-muted/40 text-muted-foreground">
                <Plug class="h-4 w-4" />
              </span>
              <h2 class="type-heading">Provider connection</h2>
            </div>
            <div class="space-y-4 px-5 pb-5 pt-3">
              <div class="space-y-2">
                <label class="type-field-label">Issuer</label>
                <input
                  v-model="issuer"
                  type="text"
                  placeholder="https://sso.example.com"
                  class="flex h-9 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground/70 focus-visible:border-foreground/35 focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring/30"
                />
                <p class="type-meta">HTTPS, no trailing slash.</p>
              </div>

              <div class="grid gap-4 sm:grid-cols-2">
                <div class="space-y-2">
                  <label class="type-field-label">Client ID</label>
                  <input
                    v-model="clientId"
                    type="text"
                    class="flex h-9 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground/70 focus-visible:border-foreground/35 focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring/30"
                  />
                </div>
                <div class="space-y-2">
                  <label class="type-field-label">Client secret</label>
                  <input
                    v-if="oidcMeta?.client_secret_configured && !replacingSecret"
                    type="password"
                    value="····························"
                    disabled
                    class="flex h-9 w-full rounded-xl border border-border bg-muted/40 px-3 text-sm text-muted-foreground"
                  />
                  <input
                    v-else
                    v-model="clientSecret"
                    type="password"
                    :placeholder="oidcMeta?.client_secret_configured ? 'New secret' : 'Paste once — stored encrypted'"
                    class="flex h-9 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground/70 focus-visible:border-foreground/35 focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring/30"
                  />
                  <p class="type-meta">
                    <template v-if="oidcMeta?.client_secret_configured && !replacingSecret">
                      Encrypted at rest.
                      <button type="button" class="underline hover:text-foreground" @click="replacingSecret = true">
                        Replace secret
                      </button>
                    </template>
                    <template v-else-if="oidcMeta?.client_secret_configured">
                      Leave blank to keep the current one.
                    </template>
                  </p>
                </div>
              </div>

              <div class="space-y-2">
                <label class="type-field-label">Scopes</label>
                <input
                  v-model="scopesInput"
                  type="text"
                  class="flex h-9 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground focus-visible:border-foreground/35 focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring/30"
                />
                <p class="type-meta">Must include <span class="font-mono">openid</span>.</p>
              </div>

              <div class="grid gap-4 sm:grid-cols-2">
                <div class="space-y-2">
                  <label class="type-field-label" for="sso-token-auth">Token endpoint auth</label>
                  <select id="sso-token-auth" v-model="tokenAuth" class="field-select w-full">
                    <option value="auto">Auto (from discovery)</option>
                    <option value="client_secret_basic">Client secret — Basic</option>
                    <option value="client_secret_post">Client secret — POST body</option>
                  </select>
                  <p class="type-meta">
                    <template v-if="tokenAuth === 'auto'">
                      Negotiated from discovery<template v-if="probe?.client_auth">
                        — currently <span class="font-mono">{{ probe.client_auth }}</span></template
                      >. Nothing declared falls back to Basic (RFC 6749 §2.3.1).
                    </template>
                    <template v-else>Overrides discovery for every token request.</template>
                  </p>
                </div>
                <div class="space-y-2">
                  <label class="type-field-label" for="sso-groups-claim">Groups claim</label>
                  <input
                    id="sso-groups-claim"
                    v-model="groupsClaim"
                    type="text"
                    placeholder="groups"
                    class="flex h-9 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground/70 focus-visible:border-foreground/35 focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring/30"
                  />
                  <p class="type-meta">
                    Mirrored for display and audit only — never grants permissions.
                  </p>
                </div>
              </div>

              <p v-if="validationError" class="field-error">{{ validationError }}</p>

              <div class="flex flex-wrap items-center gap-3 pt-1">
                <UiButton type="button" :disabled="saving" @click="save">
                  {{ saving ? 'Saving…' : 'Save & apply' }}
                </UiButton>
                <UiButton
                  type="button"
                  variant="outline"
                  :disabled="testing || !issuer.trim()"
                  @click="testConnection"
                >
                  {{ testing ? 'Testing…' : 'Test connection' }}
                </UiButton>
              </div>

              <!-- probe result (persistent until the next test or save) -->
              <div
                v-if="probe"
                class="mt-4 rounded-xl p-4"
                :class="
                  !probe.ok
                    ? 'bg-destructive/10'
                    : probe.issuer_match === false
                      ? 'bg-warning/10'
                      : 'bg-muted/40'
                "
              >
                <template v-if="probe.ok">
                  <p
                    v-if="probe.issuer_match !== false"
                    class="flex items-center gap-1.5 text-sm font-medium text-success"
                  >
                    <Check class="h-3.5 w-3.5" />
                    Discovery OK
                  </p>
                  <p
                    v-else
                    class="flex items-center gap-1.5 text-sm font-medium text-warning"
                  >
                    <AlertTriangle class="h-3.5 w-3.5" />
                    Issuer mismatch — sign-in stays inactive until they match
                  </p>
                  <div class="mt-2 grid grid-cols-[110px_minmax(0,1fr)] items-baseline gap-x-3 gap-y-1 text-xs">
                    <span class="text-muted-foreground">Issuer match</span>
                    <span>{{ probe.issuer_match ? 'yes' : `no — provider reports ${probe.discovered_issuer}` }}</span>
                    <span class="text-muted-foreground">Token endpoint</span>
                    <span class="truncate font-mono text-xs" :title="probe.token_endpoint">{{ probe.token_endpoint }}</span>
                    <span class="text-muted-foreground">End-session</span>
                    <span>
                      {{ probe.end_session_available ? 'available — sign-out ends the provider session' : 'not advertised — sign-out is local only' }}
                    </span>
                    <span class="text-muted-foreground">Client auth</span>
                    <span class="font-mono text-xs">{{ probe.client_auth }}</span>
                  </div>
                </template>
                <template v-else>
                  <p class="text-sm font-medium">Discovery failed</p>
                  <p class="mt-1 text-xs text-muted-foreground">{{ probe.error }}</p>
                </template>
              </div>

              <p v-if="notice" class="notice-success text-sm">{{ notice }}</p>
              <p v-if="error" class="field-error">{{ error }}</p>
            </div>
          </div>

          <!-- registration: always in the main column, with the wide rows -->
          <div class="surface-card">
            <div class="flex items-center gap-2.5 px-5 pt-4">
              <span class="flex h-[30px] w-[30px] flex-none items-center justify-center rounded-lg bg-muted/40 text-muted-foreground">
                <Link2 class="h-4 w-4" />
              </span>
              <div>
                <h2 class="type-heading">Register at your provider</h2>
                <p class="type-heading-desc">Copy these into your provider's app registration.</p>
              </div>
            </div>
            <div class="px-5 pb-4 pt-2">
              <div
                v-for="uri in registrationUris"
                :key="uri.key"
                class="-mx-2 flex items-center gap-3 rounded-lg px-2 py-2 transition-colors hover:bg-muted/40"
              >
                <div class="w-40 flex-none">
                  <p class="text-sm font-medium">{{ uri.label }}</p>
                  <p class="text-xs text-muted-foreground">{{ uri.what }}</p>
                </div>
                <code
                  class="min-w-0 flex-1 overflow-hidden overflow-ellipsis whitespace-nowrap surface-inset px-2.5 py-1.5 font-mono text-xs"
                  :title="uri.value"
                >
                  {{ uri.value || 'available after the issuer is saved' }}
                </code>
                <UiButton
                  v-if="uri.value"
                  variant="outline"
                  size="sm"
                  class="w-[30px] px-0"
                  :class="copiedKey === uri.key ? 'bg-success/10 text-success' : 'text-muted-foreground'"
                  :title="copiedKey === uri.key ? 'Copied' : 'Copy'"
                  @click="copyUri(uri.key, uri.value)"
                >
                  <Check v-if="copiedKey === uri.key" class="h-3.5 w-3.5" />
                  <Copy v-else class="h-3.5 w-3.5" />
                </UiButton>
              </div>

              <details class="mt-2">
                <summary class="cursor-pointer text-xs text-muted-foreground hover:text-foreground">
                  If a URL isn't registered
                </summary>
                <ul class="mt-2 list-disc space-y-1 pl-5 text-xs text-muted-foreground">
                  <li>Callback missing → sign-in fails.</li>
                  <li>Back-channel missing → an IdP-side disable waits for JWT expiry (the session lifetime).</li>
                  <li>Landing missing → the browser stays on the provider page after sign-out.</li>
                </ul>
              </details>
            </div>
          </div>
        </div>

        <!-- sidebar (320px) -->
        <div class="min-w-0">
          <!-- sign-in policy (always present; the defaults are the recommendation) -->
          <div class="surface-card">
            <div class="flex items-center gap-2.5 px-4 pt-4">
              <span class="flex h-[30px] w-[30px] flex-none items-center justify-center rounded-lg bg-muted/40 text-muted-foreground">
                <ShieldCheck class="h-4 w-4" />
              </span>
              <h2 class="type-heading">Sign-in policy</h2>
            </div>
            <div class="px-4 pb-4 pt-2">
              <p class="type-heading-desc">
                <template v-if="firstTime">
                  What a colleague gets on their first SSO sign-in. New identities only — this
                  rides along with your first Save.
                </template>
                <template v-else>
                  What a colleague sees on their first SSO sign-in. New identities only — picking
                  a policy saves it right away.
                </template>
              </p>

              <label
                class="flex cursor-pointer gap-3 rounded-lg border p-3.5 transition-colors hover:bg-muted/40"
                :class="autoProvision ? 'border-foreground bg-muted/40' : 'border-border'"
              >
                <input
                  type="radio"
                  :checked="autoProvision"
                  :disabled="savingPolicy"
                  class="mt-0.5 accent-foreground disabled:opacity-50"
                  @change="setPolicy(true)"
                />
                <span>
                  <span class="inline-flex items-center gap-2 text-sm font-medium">
                    Create on first sign-in
                    <span class="chip-neutral">Recommended</span>
                  </span>
                  <span class="mt-0.5 block text-xs text-muted-foreground">
                    They sign in immediately — a plain account, no teams, no admin. Grant roles afterwards.
                  </span>
                </span>
              </label>
              <label
                class="mt-4 flex cursor-pointer gap-3 rounded-lg border p-3.5 transition-colors hover:bg-muted/40"
                :class="!autoProvision ? 'border-foreground bg-muted/40' : 'border-border'"
              >
                <input
                  type="radio"
                  :checked="!autoProvision"
                  :disabled="savingPolicy"
                  class="mt-0.5 accent-foreground disabled:opacity-50"
                  @change="setPolicy(false)"
                />
                <span>
                  <span class="text-sm font-medium">Queue for approval</span>
                  <span class="mt-0.5 block text-xs text-muted-foreground">
                    They're refused and wait in the Users-page queue until you approve them.
                  </span>
                </span>
              </label>

              <p v-if="savingPolicy" class="text-xs text-muted-foreground">Saving…</p>
              <p
                v-else-if="policyNotice"
                class="flex items-center gap-1.5 text-xs font-medium text-success"
              >
                <Check class="h-3 w-3" />
                {{ policyNotice }}
              </p>
              <p v-if="policyError" class="field-error">{{ policyError }}</p>

              <button
                v-if="!autoProvision && pendingCount > 0"
                type="button"
                class="mt-3 inline-flex w-full items-center gap-1.5 notice-warning px-3 py-2 text-xs font-medium hover:bg-muted/40"
                @click="router.push('/users')"
              >
                <Clock class="h-3.5 w-3.5" />
                {{ pendingCount }} awaiting approval
                <span class="ml-auto opacity-75">→ Users &amp; teams</span>
              </button>

              <p class="mt-3 border-t border-border/40 pt-3 text-xs text-muted-foreground">
                SSO accounts have no local password. Your admin login stays as break-glass.
              </p>
            </div>
          </div>

          <!-- access policy (global: local and SSO sign-ins) -->
          <div class="surface-card mt-6">
            <div class="flex items-center gap-2.5 px-4 pt-4">
              <span class="flex h-[30px] w-[30px] flex-none items-center justify-center rounded-lg bg-muted/40 text-muted-foreground">
                <Clock class="h-4 w-4" />
              </span>
              <h2 class="type-heading">Access policy</h2>
            </div>
            <div class="px-4 pb-4 pt-2">
              <p class="type-heading-desc">
                Applies to every sign-in, local and SSO. Changing the lifetime affects tokens
                issued afterwards — existing sessions keep theirs.
              </p>

              <div class="mt-4 space-y-2">
                <label class="type-field-label" for="sso-session-ttl">Session lifetime (hours)</label>
                <input
                  id="sso-session-ttl"
                  v-model.number="policyTtl"
                  type="number"
                  min="1"
                  max="720"
                  class="flex h-9 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground focus-visible:border-foreground/35 focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring/30"
                />
                <p class="type-meta">
                  Default {{ policy?.ttl_default_hours ?? 24 }} h
                  <template v-if="policy?.ttl_overridden"> · currently overridden</template>.
                </p>
              </div>

              <div class="mt-4 space-y-2">
                <label class="type-field-label" for="sso-session-idle">Idle timeout (minutes)</label>
                <input
                  id="sso-session-idle"
                  v-model.number="policyIdle"
                  type="number"
                  min="0"
                  max="1440"
                  class="flex h-9 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground focus-visible:border-foreground/35 focus-visible:outline-hidden focus-visible:ring-2 focus-visible:ring-ring/30"
                />
                <p class="type-meta">
                  0 = off. Signs this browser out after inactivity; the token itself stays valid
                  until its session lifetime ends.
                </p>
              </div>

              <div class="mt-4 flex flex-wrap items-center gap-3">
                <UiButton type="button" :disabled="savingAccess" @click="savePolicy">
                  {{ savingAccess ? 'Saving…' : 'Save policy' }}
                </UiButton>
                <button
                  type="button"
                  class="text-xs text-muted-foreground underline hover:text-foreground disabled:opacity-50"
                  :disabled="savingAccess"
                  @click="resetPolicy"
                >
                  Reset to defaults
                </button>
              </div>

              <p
                v-if="accessNotice"
                class="mt-2 flex items-center gap-1.5 text-xs font-medium text-success"
              >
                <Check class="h-3 w-3" />
                {{ accessNotice }}
              </p>
              <p v-if="accessError" class="field-error mt-2">{{ accessError }}</p>
            </div>
          </div>
        </div>
      </div>

      <!-- recent authentication events (super's UiDataTable recipe:
           no card padding, one 20px gutter, 11px column headers) -->
      <div class="surface-card overflow-hidden">
        <div class="px-5 pt-5">
          <h2 class="type-heading">Recent authentication events</h2>
          <p class="type-heading-desc mt-1">
            SSO sign-ins, provisioning, approvals, rejections, and provider sign-outs.
          </p>
        </div>

        <p v-if="eventsError" class="field-error mx-5 mt-4">{{ eventsError }}</p>

        <div class="overflow-x-auto">
          <table class="w-full text-sm">
            <thead>
              <tr class="border-b border-border/40">
                <th class="w-40 px-5 py-2.5 text-left text-[11px] font-medium uppercase tracking-[0.04em] text-muted-foreground/80">
                  Time
                </th>
                <th class="w-44 px-5 py-2.5 text-left text-[11px] font-medium uppercase tracking-[0.04em] text-muted-foreground/80">
                  Event
                </th>
                <th class="px-5 py-2.5 text-left text-[11px] font-medium uppercase tracking-[0.04em] text-muted-foreground/80">
                  User
                </th>
                <th class="w-36 px-5 py-2.5 text-left text-[11px] font-medium uppercase tracking-[0.04em] text-muted-foreground/80">
                  IP
                </th>
                <th class="px-5 py-2.5 text-left text-[11px] font-medium uppercase tracking-[0.04em] text-muted-foreground/80">
                  Detail
                </th>
              </tr>
            </thead>
            <tbody v-if="eventsLoading && events.length === 0">
              <tr v-for="n in 3" :key="n" class="border-b border-border/40 last:border-0">
                <td colspan="5" class="px-5 py-3">
                  <div class="h-4 w-full animate-pulse rounded bg-muted/40"></div>
                </td>
              </tr>
            </tbody>
            <tbody v-else-if="events.length === 0">
              <tr>
                <td colspan="5" class="px-5 py-12 text-center text-sm text-muted-foreground">
                  No authentication events yet
                </td>
              </tr>
            </tbody>
            <tbody v-else>
              <tr
                v-for="event in events"
                :key="event.id"
                class="border-b border-border/40 last:border-0 hover:bg-muted/40"
              >
                <td class="whitespace-nowrap px-5 py-3 text-xs tabular-nums text-muted-foreground">
                  {{ formatEventTime(event.created_at) }}
                </td>
                <td class="px-5 py-3">
                  <span :class="eventChip(event.action)">{{ eventLabel(event.action) }}</span>
                </td>
                <td class="px-5 py-3 text-xs">{{ eventUser(event) }}</td>
                <td class="px-5 py-3 text-xs text-muted-foreground">{{ event.ip || '—' }}</td>
                <td class="px-5 py-3 text-xs text-muted-foreground">
                  <span class="block max-w-[28rem] truncate">
                    {{ eventDetail(event) }}
                  </span>
                </td>
              </tr>
            </tbody>
          </table>
        </div>

        <TablePagination
          v-model:page="eventsPage"
          v-model:pageSize="eventsPageSize"
          :total="eventsTotal"
          :page-count="eventsPageCount"
          :range-label="eventsRangeLabel"
        />
      </div>
    </template>
  </div>
</template>
