<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { oidcSettingsApi, type OidcSettings, type DiscoveryProbe } from '@/api/oidcSettings'
import UiButton from '@/components/ui/UiButton.vue'

// Web-editable OIDC SSO (Phase 1.5): every field lives in the database once
// saved (DB > manager.toml seed) and takes effect immediately — no restart.
// The client secret is write-only: the API never echoes it back.
const loading = ref(true)
const saving = ref(false)
const testing = ref(false)
const resetting = ref(false)

const enabled = ref(false)
const issuer = ref('')
const clientId = ref('')
const clientSecret = ref('') // write-only: empty = keep the stored one
const redirectOverride = ref('')
const scopesInput = ref('openid, profile, email')
const autoProvision = ref(true)

const source = ref<'database' | 'file'>('file')
const active = ref(false)
const updatedBy = ref('')
const validationError = ref('')
const probe = ref<DiscoveryProbe | null>(null)
const notice = ref('')
const error = ref('')

const scopes = computed(() =>
  scopesInput.value
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean),
)

const redirectDisplay = computed(() => {
  if (redirectOverride.value.trim()) return redirectOverride.value.trim()
  return (
    oidcMeta.value?.derived_redirect_uri ??
    `${oidcMeta.value?.public_base_url ?? ''}/api/v1/auth/oidc/callback`
  )
})

const oidcMeta = ref<OidcSettings | null>(null)

function apply(settings: OidcSettings) {
  oidcMeta.value = settings
  source.value = settings.source
  enabled.value = settings.enabled
  issuer.value = settings.issuer
  clientId.value = settings.client_id
  clientSecret.value = ''
  redirectOverride.value = settings.redirect_uri_override ?? ''
  scopesInput.value = settings.scopes.join(', ')
  autoProvision.value = settings.auto_provision
  active.value = settings.active
  updatedBy.value = settings.updated_by
  validationError.value =
    settings.validation.ok === false ? (settings.validation.error ?? '') : ''
}

onMounted(async () => {
  try {
    apply(await oidcSettingsApi.get())
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
  } catch (e: any) {
    probe.value = { ok: false, error: e.message || 'Probe failed' }
  } finally {
    testing.value = false
  }
}

async function save() {
  saving.value = true
  error.value = ''
  notice.value = ''
  try {
    const settings = await oidcSettingsApi.update({
      enabled: enabled.value,
      issuer: issuer.value.trim(),
      client_id: clientId.value.trim(),
      client_secret: clientSecret.value.trim() || undefined,
      redirect_uri_override: redirectOverride.value.trim() || null,
      scopes: scopes.value,
      auto_provision: autoProvision.value,
    })
    apply(settings)
    probe.value = null
    notice.value = 'SSO settings saved and applied live.'
  } catch (e: any) {
    error.value = e.message || 'Save failed'
  } finally {
    saving.value = false
  }
}

async function resetToFile() {
  resetting.value = true
  error.value = ''
  notice.value = ''
  try {
    apply(await oidcSettingsApi.reset())
    notice.value = 'Reverted to the manager.toml seed.'
  } catch (e: any) {
    error.value = e.message || 'Reset failed'
  } finally {
    resetting.value = false
  }
}
</script>

<template>
  <div class="mx-auto max-w-3xl space-y-6 p-6">
    <div class="space-y-1">
      <h1 class="text-lg font-semibold tracking-tight">Single sign-on (OIDC)</h1>
      <p class="type-meta">
        Sign-in through any OIDC provider. Local admin login always remains
        available as the break-glass path.
      </p>
    </div>

    <div
      v-if="loading"
      class="rounded-xl border border-border bg-card p-6 text-sm text-muted-foreground"
    >
      Loading…
    </div>

    <template v-else>
      <!-- Status -->
      <div class="flex flex-wrap items-center gap-3 rounded-xl border border-border bg-card p-4">
        <span
          class="inline-flex items-center gap-1.5 rounded-full px-2.5 py-0.5 text-xs font-medium"
          :class="active ? 'bg-success/15 text-success' : 'bg-muted text-muted-foreground'"
        >
          <span class="h-1.5 w-1.5 rounded-full" :class="active ? 'bg-success' : 'bg-muted-foreground'" />
          {{ active ? 'Active — sign-in enabled' : 'Inactive' }}
        </span>
        <span class="text-xs text-muted-foreground">
          Config source: <span class="font-medium">{{ source === 'database' ? 'database (web)' : 'manager.toml seed' }}</span>
        </span>
        <span v-if="updatedBy" class="text-xs text-muted-foreground">Last saved by {{ updatedBy }}</span>
      </div>

      <!-- Form -->
      <div class="space-y-4 rounded-xl border border-border bg-card p-5">
        <label class="flex items-center gap-3">
          <input v-model="enabled" type="checkbox" class="h-4 w-4 accent-primary" />
          <span class="text-sm font-medium">Enable SSO sign-in</span>
        </label>

        <div class="space-y-1.5">
          <label class="type-label">Issuer</label>
          <input
            v-model="issuer"
            type="text"
            placeholder="https://sso.example.com"
            class="flex h-10 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground focus-visible:outline-hidden focus-visible:border-foreground/35 focus-visible:ring-2 focus-visible:ring-ring/30"
          />
          <p class="type-meta">HTTPS origin, no trailing slash (plain http only for localhost).</p>
        </div>

        <div class="grid gap-4 sm:grid-cols-2">
          <div class="space-y-1.5">
            <label class="type-label">Client ID</label>
            <input
              v-model="clientId"
              type="text"
              placeholder="buz"
              class="flex h-10 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground focus-visible:outline-hidden focus-visible:border-foreground/35 focus-visible:ring-2 focus-visible:ring-ring/30"
            />
          </div>
          <div class="space-y-1.5">
            <label class="type-label">Client secret</label>
            <input
              v-model="clientSecret"
              type="password"
              :placeholder="oidcMeta?.client_secret_configured ? 'Configured — leave blank to keep' : 'Enter the provider secret'"
              class="flex h-10 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground focus-visible:outline-hidden focus-visible:border-foreground/35 focus-visible:ring-2 focus-visible:ring-ring/30"
            />
            <p class="type-meta">Stored encrypted at rest; never displayed.</p>
          </div>
        </div>

        <div class="space-y-1.5">
          <label class="type-label">Redirect URI override <span class="text-muted-foreground">(optional)</span></label>
          <input
            v-model="redirectOverride"
            type="text"
            :placeholder="`Derived: ${redirectDisplay}`"
            class="flex h-10 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground placeholder:text-muted-foreground focus-visible:outline-hidden focus-visible:border-foreground/35 focus-visible:ring-2 focus-visible:ring-ring/30"
          />
          <p class="type-meta">
            Effective callback (register this exact string at the provider):
            <span class="font-mono">{{ redirectDisplay }}</span>
          </p>
          <p v-if="oidcMeta?.backchannel_logout_uri" class="type-meta">
            Back-channel logout (register to revoke sessions immediately):
            <span class="font-mono">{{ oidcMeta.backchannel_logout_uri }}</span>
          </p>
          <p v-if="oidcMeta?.post_logout_redirect_uri" class="type-meta">
            Post-logout landing (register to end the provider session on sign-out):
            <span class="font-mono">{{ oidcMeta.post_logout_redirect_uri }}</span>
          </p>
        </div>

        <div class="grid gap-4 sm:grid-cols-2">
          <div class="space-y-1.5">
            <label class="type-label">Scopes (comma-separated)</label>
            <input
              v-model="scopesInput"
              type="text"
              class="flex h-10 w-full rounded-xl border border-border bg-background px-3 text-sm text-foreground focus-visible:outline-hidden focus-visible:border-foreground/35 focus-visible:ring-2 focus-visible:ring-ring/30"
            />
            <p class="type-meta">Must include <span class="font-mono">openid</span>.</p>
          </div>
          <label class="flex items-end gap-3 pb-2">
            <input v-model="autoProvision" type="checkbox" class="h-4 w-4 accent-primary" />
            <span class="text-sm"
              >Auto-provision users on first sign-in
              <span class="block text-xs text-muted-foreground">
                Plain accounts, no teams or admin rights; off = refuse unbound identities.
              </span>
            </span>
          </label>
        </div>

        <p v-if="validationError" class="field-error">{{ validationError }}</p>

        <div class="flex flex-wrap items-center gap-3 pt-1">
          <UiButton type="button" :disabled="saving" @click="save">
            {{ saving ? 'Saving…' : 'Save and apply' }}
          </UiButton>
          <UiButton
            type="button"
            variant="outline"
            :disabled="testing || !issuer.trim()"
            @click="testConnection"
          >
            {{ testing ? 'Testing…' : 'Test connection' }}
          </UiButton>
          <UiButton
            v-if="source === 'database'"
            type="button"
            variant="ghost"
            :disabled="resetting"
            @click="resetToFile"
          >
            {{ resetting ? 'Reverting…' : 'Revert to manager.toml' }}
          </UiButton>
        </div>

        <!-- Probe result -->
        <div
          v-if="probe"
          class="rounded-xl border p-4 text-sm"
          :class="probe.ok ? 'border-success/40 bg-success/5' : 'border-destructive/40 bg-destructive/5'"
        >
          <template v-if="probe.ok">
            <p class="font-medium">Discovery OK</p>
            <p class="type-meta mt-1">
              Issuer match: {{ probe.issuer_match ? 'yes' : 'no — provider reports ' + probe.discovered_issuer }}
            </p>
            <p class="type-meta font-mono">{{ probe.token_endpoint }}</p>
          </template>
          <template v-else>
            <p class="font-medium">Discovery failed</p>
            <p class="type-meta mt-1">{{ probe.error }}</p>
          </template>
        </div>

        <p v-if="notice" class="text-sm text-success">{{ notice }}</p>
        <p v-if="error" class="field-error">{{ error }}</p>
      </div>

      <!-- Operator notes -->
      <div class="rounded-xl border border-border bg-card p-5 text-sm text-muted-foreground">
        <p class="font-medium text-foreground">How it works</p>
        <ul class="mt-2 list-disc space-y-1 pl-5">
          <li>Saved settings live in the database and apply immediately — no restart.</li>
          <li>First sign-in of an unknown identity creates a plain account (no teams, no admin) when auto-provision is on; grant team roles via RBAC afterwards.</li>
          <li>Accounts created through SSO cannot use password login; local admin login always works.</li>
          <li>Changing these settings is audited (<span class="font-mono">setting.update</span> / <span class="font-mono">setting.reset</span>).</li>
        </ul>
      </div>
    </template>
  </div>
</template>
