<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import { BookOpen } from '@lucide/vue'
import UiButton from '@/components/ui/UiButton.vue'
import { useAppDialog } from '@/composables/useAppDialog'
import { oidcSettingsApi, type DiscoveryProbe, type OidcSettings } from '@/api/oidcSettings'
import { oidcIdentitiesApi } from '@/api/oidcIdentities'

// SSO settings, database-only configuration (see platform/oidc-sso.md):
// one connection card, the sign-in policy as its own decision, the three
// provider registrations with copy buttons, and a clear-configuration escape
// hatch in the status header. The client secret is write-only — the API
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

let nowTick = 0
const tick = ref(0)
const timer = window.setInterval(() => {
  tick.value = ++nowTick
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
  <div class="space-y-6">
    <div class="space-y-1">
      <h1 class="text-lg font-semibold tracking-tight">Single sign-on (OIDC)</h1>
      <p class="type-meta">Sign in through any OIDC provider.</p>
    </div>

    <div
      v-if="loading"
      class="rounded-xl border border-border bg-card p-6 text-sm text-muted-foreground"
    >
      Loading…
    </div>

    <template v-else>
      <!-- status header -->
      <div class="flex flex-wrap items-center gap-3 rounded-xl border border-border bg-card p-4">
        <span
          class="inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium"
          :class="active ? 'bg-success/15 text-success' : 'bg-muted text-muted-foreground'"
        >
          <span class="h-1.5 w-1.5 rounded-full" :class="active ? 'bg-success' : 'bg-muted-foreground'" />
          {{ active ? 'Active' : 'Inactive' }}
        </span>
        <label class="inline-flex items-center gap-2 text-sm font-medium">
          <input v-model="enabled" type="checkbox" class="h-4 w-4 accent-primary" />
          Sign-in enabled
        </label>
        <span
          v-if="probe?.ok && lastProbeAt"
          class="inline-flex items-center gap-1.5 rounded-full border border-success/40 bg-success/10 px-2.5 py-0.5 text-xs font-medium text-success"
        >
          ✓ Connection OK · {{ probeAge }}
        </span>
        <span class="text-xs text-muted-foreground">Source: database</span>
        <span v-if="updatedBy" class="text-xs text-muted-foreground">Saved by {{ updatedBy }}</span>
        <UiButton v-if="savedOnce" variant="ghost" size="sm" class="ml-auto" @click="clearConfiguration">
          Clear configuration
        </UiButton>
      </div>

      <!-- provider connection -->
      <div class="space-y-4 rounded-xl border border-border bg-card p-5">
        <div class="flex items-center justify-between">
          <h2 class="text-sm font-semibold">Provider connection</h2>
          <a
            class="inline-flex items-center gap-1.5 rounded-lg px-2 py-1 text-xs text-muted-foreground hover:bg-muted/50 hover:text-foreground"
            :href="DOCS_URL"
            target="_blank"
            rel="noopener"
          >
            <BookOpen class="h-3.5 w-3.5" />
            Docs
          </a>
        </div>

        <div class="space-y-1.5">
          <label class="type-label">Issuer</label>
          <input
            v-model="issuer"
            type="text"
            placeholder="https://sso.example.com"
            class="flex h-10 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground focus-visible:outline-hidden focus-visible:border-foreground/35 focus-visible:ring-2 focus-visible:ring-ring/30"
          />
          <p class="type-meta">HTTPS, no trailing slash.</p>
        </div>

        <div class="grid gap-4 sm:grid-cols-2">
          <div class="space-y-1.5">
            <label class="type-label">Client ID</label>
            <input
              v-model="clientId"
              type="text"
              class="flex h-10 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground focus-visible:outline-hidden focus-visible:border-foreground/35 focus-visible:ring-2 focus-visible:ring-ring/30"
            />
          </div>
          <div class="space-y-1.5">
            <label class="type-label">Client secret</label>
            <input
              v-if="oidcMeta?.client_secret_configured && !replacingSecret"
              type="password"
              value="····························"
              disabled
              class="flex h-10 w-full rounded-xl border border-border bg-muted/30 px-3 text-sm text-muted-foreground"
            />
            <input
              v-else
              v-model="clientSecret"
              type="password"
              :placeholder="oidcMeta?.client_secret_configured ? 'New secret' : 'Paste once — stored encrypted'"
              class="flex h-10 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground focus-visible:outline-hidden focus-visible:border-foreground/35 focus-visible:ring-2 focus-visible:ring-ring/30"
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
            class="flex h-10 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground focus-visible:outline-hidden focus-visible:border-foreground/35 focus-visible:ring-2 focus-visible:ring-ring/30"
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
          class="rounded-xl border p-4 text-sm"
          :class="probe.ok ? 'border-success/40 bg-success/5' : 'border-destructive/40 bg-destructive/5'"
        >
          <template v-if="probe.ok">
            <p class="font-medium">
              ✓ Discovery OK
              <span v-if="probe.issuer_match === false" class="text-muted-foreground">
                — provider reports {{ probe.discovered_issuer }}
              </span>
            </p>
            <p class="type-meta font-mono">{{ probe.token_endpoint }}</p>
            <p class="type-meta">
              End-session {{ probe.end_session_available ? 'available — sign-out ends the provider session' : 'not advertised — sign-out is local only' }}
            </p>
            <p class="type-meta">
              Client auth <span class="font-mono">{{ probe.client_auth }}</span>
            </p>
          </template>
          <template v-else>
            <p class="font-medium">Discovery failed</p>
            <p class="type-meta mt-1">{{ probe.error }}</p>
          </template>
        </div>

        <p v-if="notice" class="text-sm text-success">{{ notice }}</p>
        <p v-if="error" class="field-error">{{ error }}</p>
      </div>

      <!-- sign-in policy (once connected) -->
      <div v-if="!firstTime" class="space-y-3 rounded-xl border border-border bg-card p-5">
        <h2 class="text-sm font-semibold">Sign-in policy</h2>
        <p class="type-meta">
          What a colleague sees on their first SSO sign-in. New identities only — change anytime.
        </p>

        <label
          class="flex cursor-pointer gap-3 rounded-lg border p-4"
          :class="autoProvision ? 'border-foreground ring-1 ring-foreground' : 'border-border'"
        >
          <input v-model="autoProvision" type="radio" :value="true" name="signin-policy" class="mt-1" />
          <span>
            <span class="inline-flex items-center gap-2 text-sm font-medium">
              Create on first sign-in
              <span class="rounded-full bg-muted px-2 py-0.5 text-[10px] font-medium text-muted-foreground">Recommended</span>
            </span>
            <span class="mt-0.5 block text-xs text-muted-foreground">
              They sign in immediately — a plain account, no teams, no admin. Grant roles afterwards.
            </span>
          </span>
        </label>
        <label
          class="flex cursor-pointer gap-3 rounded-lg border p-4"
          :class="!autoProvision ? 'border-foreground ring-1 ring-foreground' : 'border-border'"
        >
          <input v-model="autoProvision" type="radio" :value="false" name="signin-policy" class="mt-1" />
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
          class="inline-flex items-center gap-1.5 rounded-lg border border-amber-500/40 bg-amber-500/10 px-3 py-1.5 text-xs font-medium text-amber-500 hover:bg-amber-500/15"
          @click="router.push('/users')"
        >
          ⏳ {{ pendingCount }} awaiting approval → Users
        </button>

        <p class="type-meta">
          SSO accounts have no local password. Your admin login stays as break-glass.
        </p>
      </div>
      <p v-else class="type-meta">
        New sign-ins get a plain account automatically — you can require approval later.
      </p>

      <!-- registration -->
      <div class="rounded-xl border border-border bg-card p-5">
        <h2 class="text-sm font-semibold">Register at your provider</h2>
        <p class="type-meta mt-1">Copy these into your provider's app registration.</p>

        <div
          v-for="uri in registrationUris"
          :key="uri.key"
          class="mt-3 flex items-center gap-3 border-t border-border/50 py-3 first:border-t-0 first:pt-0 sm:mt-0 sm:py-2"
        >
          <div class="w-44 flex-none">
            <p class="text-sm font-medium">{{ uri.label }}</p>
            <p class="text-[11px] text-muted-foreground">{{ uri.what }}</p>
          </div>
          <code
            class="min-w-0 flex-1 overflow-hidden overflow-ellipsis whitespace-nowrap rounded-md bg-muted/50 px-2.5 py-1.5 font-mono text-xs"
            :title="uri.value"
          >
            {{ uri.value || 'available after the issuer is saved' }}
          </code>
          <UiButton
            v-if="uri.value"
            variant="outline"
            size="sm"
            @click="copyUri(uri.key, uri.value)"
          >
            {{ copiedKey === uri.key ? 'Copied' : 'Copy' }}
          </UiButton>
        </div>

        <details class="mt-3">
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
    </template>
  </div>
</template>
