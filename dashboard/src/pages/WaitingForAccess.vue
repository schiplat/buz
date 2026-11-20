<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { Clock, LogOut, RefreshCw } from '@lucide/vue'
import UiButton from '@/components/ui/UiButton.vue'
import { useAuthStore } from '@/stores/auth'

/**
 * Landing page for authenticated accounts that hold no authorization yet:
 * a fresh SSO account (auto-provision) or a user whose last team was removed.
 * The shell (and therefore the menu) stays closed until a team + role is
 * granted; this page re-checks `/me` so access appears without a re-login.
 */

const auth = useAuthStore()
const route = useRoute()
const router = useRouter()

const checking = ref(false)
const stillWaiting = ref(false)
let timer: ReturnType<typeof setInterval> | null = null

const redirect = computed(() => {
  const raw = route.query.redirect
  if (typeof raw === 'string' && raw.startsWith('/') && !raw.startsWith('//') && raw !== '/waiting-for-access') {
    return raw
  }
  return '/dashboard'
})

function hasAccess() {
  return (
    auth.isPlatformAdmin ||
    auth.isSuperAdmin ||
    auth.isOps ||
    auth.teams.length > 0
  )
}

async function check() {
  if (checking.value) return
  checking.value = true
  stillWaiting.value = false
  try {
    await auth.fetchMe()
    if (hasAccess()) {
      await router.replace(redirect.value)
      return
    }
    stillWaiting.value = true
  } finally {
    checking.value = false
  }
}

function onFocus() {
  void check()
}

function signOut() {
  auth.logout()
  // The backend picks the exit: SSO deployments end the provider session too,
  // plain deployments land on the login page.
  window.location.assign('/api/v1/auth/oidc/logout')
}

onMounted(() => {
  timer = setInterval(() => void check(), 30_000)
  window.addEventListener('focus', onFocus)
})

onUnmounted(() => {
  if (timer) clearInterval(timer)
  window.removeEventListener('focus', onFocus)
})
</script>

<template>
  <div class="flex min-h-screen items-center justify-center bg-background px-6 py-12">
    <div class="w-full max-w-md rounded-2xl border border-border/50 bg-card p-8 text-center shadow-xs">
      <div class="mx-auto mb-4 flex h-11 w-11 items-center justify-center rounded-xl bg-muted text-muted-foreground">
        <Clock class="h-5 w-5" />
      </div>

      <h1 class="text-lg font-semibold tracking-tight">Waiting for access</h1>
      <p class="mt-2 text-sm text-muted-foreground">
        You're signed in as
        <span class="font-mono text-[13px] text-foreground">{{ auth.username }}</span>,
        but your account isn't in a team yet.
      </p>
      <p class="mt-2 text-sm text-muted-foreground">
        A platform admin or team owner needs to assign you a team and role before you can use Buz.
      </p>

      <div class="mt-6 flex justify-center gap-2">
        <UiButton size="sm" :disabled="checking" @click="check">
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': checking }" />
          {{ checking ? 'Checking…' : 'Check again' }}
        </UiButton>
        <UiButton variant="ghost" size="sm" @click="signOut">
          <LogOut class="h-3.5 w-3.5" />
          Sign out
        </UiButton>
      </div>

      <p v-if="stillWaiting" class="mt-4 text-xs text-muted-foreground">
        Still no team yet — ask a platform admin to grant you access.
      </p>

      <p class="mt-6 text-[11px] leading-relaxed text-muted-foreground/70">
        Access is granted from Users &amp; teams (Ungrouped → Assign team). This page checks again automatically.
      </p>
    </div>
  </div>
</template>
