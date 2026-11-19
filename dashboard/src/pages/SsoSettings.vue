<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { BookOpen, Check, Clock, Copy, Database, Link2, Plug, RotateCcw, ShieldCheck } from '@lucide/vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import { useAppDialog } from '@/composables/useAppDialog'
import { oidcSettingsApi, type DiscoveryProbe, type OidcSettings } from '@/api/oidcSettings'
import { oidcIdentitiesApi } from '@/api/oidcIdentities'

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

const savedOnce = ref(false)
const active = ref(false)
const updatedBy = ref('')
const validationError = ref('')
const probe = ref<DiscoveryProbe | null>(null)
const lastProbeAt = ref<number | null>(null)
const replacingSecret = ref(false)
const pendingCount = ref(0)
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
  { key: 'callback', label: 'Callback', what: 'sign-in', value: oidcMeta.value?.redirect_uri_effective ?? '' },
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
})

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
</script>

<template>
  <div class="space-y-5">
    <PageHeader title="Single sign-on (OIDC)" description="Sign in through any OIDC provider.">
      <template #actions>
        <a
          class="inline-flex items-center gap-1.5 rounded-lg border border-border px-3 py-1.5 text-xs text-muted-foreground transition-colors hover:bg-muted/40 hover:text-foreground"
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
      class="rounded-xl border border-border bg-card p-6 text-sm text-muted-foreground shadow-xs"
    >
      Loading…
    </div>

    <template v-else>
      <!-- status strip: full-width once configured … -->
      <div
        v-if="!firstTime"
        class="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-xl border border-border bg-card p-4 shadow-xs"
      >
        <span
          class="inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium"
          :class="active ? 'bg-success/15 text-success' : 'bg-muted text-muted-foreground'"
        >
          <span class="h-1.5 w-1.5 rounded-full" :class="active ? 'bg-success' : 'bg-muted-foreground'" />
          {{ active ? 'Active' : 'Inactive' }}
        </span>
        <label class="inline-flex items-center gap-2 text-sm font-medium">
          <span class="relative inline-flex h-5 w-[34px]">
            <input v-model="enabled" type="checkbox" class="peer h-full w-full cursor-pointer appearance-none rounded-full bg-muted-foreground/30 transition-colors checked:bg-success" />
            <span class="pointer-events-none absolute left-0.5 top-0.5 h-4 w-4 rounded-full bg-white shadow-sm transition-all peer-checked:translate-x-[14px]"></span>
          </span>
          Sign-in enabled
        </label>
        <span
          v-if="probe?.ok && lastProbeAt"
          class="inline-flex items-center gap-1.5 rounded-full border border-success/40 bg-success/10 px-2.5 py-0.5 text-xs font-medium text-success"
        >
          <Check class="h-3 w-3" />
          Connection OK · {{ probeAge }}
        </span>
        <span class="inline-flex items-center gap-1.5 rounded-full border border-border/60 bg-muted/40 px-2.5 py-0.5 text-xs text-muted-foreground">
          <Database class="h-3 w-3" />
          Source: database
        </span>
        <span v-if="updatedBy" class="text-xs text-muted-foreground">Saved by {{ updatedBy }}</span>
        <UiButton variant="ghost" size="sm" class="ml-auto" @click="clearConfiguration">
          <RotateCcw class="h-3.5 w-3.5" />
          Clear configuration
        </UiButton>
      </div>

      <p v-if="!loading && error" class="field-error">{{ error }}</p>

      <!-- two-column composition: main column + 320px sidebar -->
      <div class="grid items-start gap-5 lg:grid-cols-[minmax(0,1fr)_320px]">
        <!-- main column -->
        <div class="flex min-w-0 flex-col gap-5">
          <!-- first setup: the strip aligns with the column instead of spanning -->
          <div
            v-if="firstTime"
            class="flex flex-wrap items-center gap-x-3 gap-y-2 rounded-xl border border-border bg-card p-4 shadow-xs"
          >
            <span
              class="inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium"
              :class="active ? 'bg-success/15 text-success' : 'bg-muted text-muted-foreground'"
            >
              <span class="h-1.5 w-1.5 rounded-full" :class="active ? 'bg-success' : 'bg-muted-foreground'" />
              {{ active ? 'Active' : 'Inactive' }}
            </span>
            <label class="inline-flex items-center gap-2 text-sm font-medium">
              <span class="relative inline-flex h-5 w-[34px]">
                <input v-model="enabled" type="checkbox" class="peer h-full w-full cursor-pointer appearance-none rounded-full bg-muted-foreground/30 transition-colors checked:bg-success" />
                <span class="pointer-events-none absolute left-0.5 top-0.5 h-4 w-4 rounded-full bg-white shadow-sm transition-all peer-checked:translate-x-[14px]"></span>
              </span>
              Sign-in enabled
            </label>
            <span class="inline-flex items-center gap-1.5 rounded-full border border-border/60 bg-muted/40 px-2.5 py-0.5 text-xs text-muted-foreground">
              <Database class="h-3 w-3" />
              No configuration yet
            </span>
          </div>

          <!-- provider connection -->
          <div class="rounded-xl border border-border bg-card shadow-xs">
            <div class="flex items-center gap-2.5 px-5 pt-4">
              <span class="flex h-[30px] w-[30px] flex-none items-center justify-center rounded-lg bg-muted/50 text-muted-foreground">
                <Plug class="h-4 w-4" />
              </span>
              <h2 class="text-sm font-semibold">Provider connection</h2>
            </div>
            <div class="space-y-4 px-5 pb-5 pt-3">
              <div class="space-y-1.5">
                <label class="type-label">Issuer</label>
                <input
                  v-model="issuer"
                  type="text"
                  placeholder="https://sso.example.com"
                  class="flex h-9 w-full rounded-lg border border-border bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground/70 focus-visible:border-foreground/35 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/25"
                />
                <p class="type-meta">HTTPS, no trailing slash.</p>
              </div>

              <div class="grid gap-4 sm:grid-cols-2">
                <div class="space-y-1.5">
                  <label class="type-label">Client ID</label>
                  <input
                    v-model="clientId"
                    type="text"
                    class="flex h-9 w-full rounded-lg border border-border bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground/70 focus-visible:border-foreground/35 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/25"
                  />
                </div>
                <div class="space-y-1.5">
                  <label class="type-label">Client secret</label>
                  <input
                    v-if="oidcMeta?.client_secret_configured && !replacingSecret"
                    type="password"
                    value="····························"
                    disabled
                    class="flex h-9 w-full rounded-lg border border-border bg-muted/30 px-3 text-sm text-muted-foreground"
                  />
                  <input
                    v-else
                    v-model="clientSecret"
                    type="password"
                    :placeholder="oidcMeta?.client_secret_configured ? 'New secret' : 'Paste once — stored encrypted'"
                    class="flex h-9 w-full rounded-lg border border-border bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground/70 focus-visible:border-foreground/35 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/25"
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

              <div class="space-y-1.5">
                <label class="type-label">Scopes</label>
                <input
                  v-model="scopesInput"
                  type="text"
                  class="flex h-9 w-full rounded-lg border border-border bg-background px-3 text-sm text-foreground focus-visible:border-foreground/35 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/25"
                />
                <p class="type-meta">Must include <span class="font-mono">openid</span>.</p>
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
                class="rounded-lg border p-4"
                :class="probe.ok ? 'border-success/40 bg-success/5' : 'border-destructive/40 bg-destructive/5'"
              >
                <template v-if="probe.ok">
                  <p class="flex items-center gap-1.5 text-sm font-medium text-success">
                    <Check class="h-3.5 w-3.5" />
                    Discovery OK
                  </p>
                  <div class="mt-2 grid grid-cols-[110px_minmax(0,1fr)] items-baseline gap-x-3 gap-y-1 text-xs">
                    <span class="text-muted-foreground">Issuer match</span>
                    <span>{{ probe.issuer_match ? 'yes' : `no — provider reports ${probe.discovered_issuer}` }}</span>
                    <span class="text-muted-foreground">Token endpoint</span>
                    <span class="truncate font-mono text-[11.5px]" :title="probe.token_endpoint">{{ probe.token_endpoint }}</span>
                    <span class="text-muted-foreground">End-session</span>
                    <span>
                      {{ probe.end_session_available ? 'available — sign-out ends the provider session' : 'not advertised — sign-out is local only' }}
                    </span>
                    <span class="text-muted-foreground">Client auth</span>
                    <span class="font-mono text-[11.5px]">{{ probe.client_auth }}</span>
                  </div>
                </template>
                <template v-else>
                  <p class="text-sm font-medium">Discovery failed</p>
                  <p class="type-meta mt-1">{{ probe.error }}</p>
                </template>
              </div>

              <p v-if="notice" class="text-sm text-success">{{ notice }}</p>
              <p v-if="error" class="field-error">{{ error }}</p>
            </div>
          </div>

          <!-- registration (configured state: main column, wide rows) -->
          <div v-if="!firstTime" class="rounded-xl border border-border bg-card shadow-xs">
            <div class="flex items-center gap-2.5 px-5 pt-4">
              <span class="flex h-[30px] w-[30px] flex-none items-center justify-center rounded-lg bg-muted/50 text-muted-foreground">
                <Link2 class="h-4 w-4" />
              </span>
              <div>
                <h2 class="text-sm font-semibold">Register at your provider</h2>
                <p class="type-meta">Copy these into your provider's app registration.</p>
              </div>
            </div>
            <div class="px-5 pb-4 pt-2">
              <div
                v-for="uri in registrationUris"
                :key="uri.key"
                class="-mx-2 flex items-center gap-3 rounded-lg px-2 py-2 transition-colors hover:bg-muted/30"
              >
                <div class="w-40 flex-none">
                  <p class="text-sm font-medium">{{ uri.label }}</p>
                  <p class="text-[11px] text-muted-foreground">{{ uri.what }}</p>
                </div>
                <code
                  class="min-w-0 flex-1 overflow-hidden overflow-ellipsis whitespace-nowrap rounded-md border border-border/50 bg-muted/50 px-2.5 py-1.5 font-mono text-xs"
                  :title="uri.value"
                >
                  {{ uri.value || 'available after the issuer is saved' }}
                </code>
                <UiButton
                  v-if="uri.value"
                  variant="outline"
                  size="sm"
                  class="w-[30px] px-0"
                  :class="copiedKey === uri.key ? 'border-success/40 bg-success/10 text-success' : 'text-muted-foreground'"
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
                  <li>Back-channel missing → an IdP-side disable waits for JWT expiry (24 h).</li>
                  <li>Landing missing → the browser stays on the provider page after sign-out.</li>
                </ul>
              </details>
            </div>
          </div>
        </div>

        <!-- sidebar (320px) -->
        <div class="min-w-0">
          <!-- sign-in policy (configured state only) -->
          <div v-if="!firstTime" class="rounded-xl border border-border bg-card shadow-xs">
            <div class="flex items-center gap-2.5 px-4 pt-4">
              <span class="flex h-[30px] w-[30px] flex-none items-center justify-center rounded-lg bg-muted/50 text-muted-foreground">
                <ShieldCheck class="h-4 w-4" />
              </span>
              <h2 class="text-sm font-semibold">Sign-in policy</h2>
            </div>
            <div class="px-4 pb-4 pt-2">
              <p class="type-meta">
                What a colleague sees on their first SSO sign-in. New identities only — change anytime.
              </p>

              <label
                class="mt-3 flex cursor-pointer gap-3 rounded-lg border p-3.5 transition-colors hover:bg-muted/20"
                :class="autoProvision ? 'border-foreground bg-muted/10' : 'border-border'"
              >
                <input v-model="autoProvision" type="radio" :value="true" name="signin-policy" class="mt-0.5 accent-foreground" />
                <span>
                  <span class="inline-flex items-center gap-2 text-sm font-medium">
                    Create on first sign-in
                    <span class="rounded-full bg-muted px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide text-muted-foreground">Recommended</span>
                  </span>
                  <span class="mt-0.5 block text-xs text-muted-foreground">
                    They sign in immediately — a plain account, no teams, no admin. Grant roles afterwards.
                  </span>
                </span>
              </label>
              <label
                class="mt-2.5 flex cursor-pointer gap-3 rounded-lg border p-3.5 transition-colors hover:bg-muted/20"
                :class="!autoProvision ? 'border-foreground bg-muted/10' : 'border-border'"
              >
                <input v-model="autoProvision" type="radio" :value="false" name="signin-policy" class="mt-0.5 accent-foreground" />
                <span>
                  <span class="text-sm font-medium">Queue for approval</span>
                  <span class="mt-0.5 block text-xs text-muted-foreground">
                    They're refused and wait in the Users-page queue until you approve them.
                  </span>
                </span>
              </label>

              <button
                v-if="!autoProvision && pendingCount > 0"
                type="button"
                class="mt-3 inline-flex w-full items-center gap-1.5 rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs font-medium text-amber-500 hover:bg-amber-500/15"
                @click="router.push('/users')"
              >
                <Clock class="h-3.5 w-3.5" />
                {{ pendingCount }} awaiting approval
                <span class="ml-auto opacity-75">→ Users</span>
              </button>

              <p class="mt-3 border-t border-border/50 pt-3 text-xs text-muted-foreground">
                SSO accounts have no local password. Your admin login stays as break-glass.
              </p>
            </div>
          </div>

          <!-- registration (first-time: narrow sidebar variant) -->
          <div v-else class="rounded-xl border border-border bg-card shadow-xs">
            <div class="flex items-center gap-2.5 px-4 pt-4">
              <span class="flex h-[30px] w-[30px] flex-none items-center justify-center rounded-lg bg-muted/50 text-muted-foreground">
                <Link2 class="h-4 w-4" />
              </span>
              <h2 class="text-sm font-semibold">Register at your provider</h2>
            </div>
            <div class="space-y-3.5 px-4 pb-4 pt-3">
              <div v-for="uri in registrationUris" :key="uri.key">
                <div class="flex items-center gap-2">
                  <span class="text-sm font-medium">{{ uri.label }}</span>
                  <span class="text-[11px] text-muted-foreground">{{ uri.what }}</span>
                  <button
                    v-if="uri.value"
                    type="button"
                    class="ml-auto flex h-[26px] w-[26px] items-center justify-center rounded-md border border-border text-muted-foreground hover:bg-muted/40 hover:text-foreground"
                    :class="copiedKey === uri.key ? 'border-success/40 bg-success/10 text-success' : ''"
                    :title="copiedKey === uri.key ? 'Copied' : 'Copy'"
                    @click="copyUri(uri.key, uri.value)"
                  >
                    <Check v-if="copiedKey === uri.key" class="h-3 w-3" />
                    <Copy v-else class="h-3 w-3" />
                  </button>
                </div>
                <code
                  class="mt-1.5 block rounded-md border border-border/50 bg-muted/50 px-2.5 py-1.5 font-mono text-[11px] leading-relaxed"
                  :class="uri.value ? 'break-all text-foreground' : 'text-muted-foreground/70'"
                >
                  {{ uri.value || 'available after the issuer is saved' }}
                </code>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- first-time: the delayed-policy note lives under the grid -->
      <p v-if="firstTime" class="type-meta">
        New sign-ins get a plain account automatically — you can require approval later.
      </p>
    </template>
  </div>
</template>
