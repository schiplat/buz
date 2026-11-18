<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useAuthStore } from '@/stores/auth'
import UiButton from '@/components/ui/UiButton.vue'

// Landing point of the OIDC callback: the Manager handed the browser a
// one-shot ticket cookie and redirected here. Exchange it for the session
// JWT, then continue to the original destination (already validated
// server-side as a same-site relative path).
const route = useRoute()
const router = useRouter()
const authStore = useAuthStore()

const status = ref<'exchanging' | 'error'>('exchanging')
const error = ref('')

const next = computed(() => {
  const raw = typeof route.query.next === 'string' ? route.query.next : '/dashboard'
  return raw.startsWith('/') && !raw.startsWith('//') ? raw : '/dashboard'
})

onMounted(async () => {
  const serverError = typeof route.query.error === 'string' ? route.query.error : ''
  if (serverError) {
    status.value = 'error'
    error.value = describeError(serverError)
    return
  }
  try {
    await authStore.oidcExchange()
    void router.push(next.value)
  } catch {
    status.value = 'error'
    error.value = describeError('exchange-failed')
  }
})

function describeError(code: string): string {
  switch (code) {
    case 'sso-disabled':
      return 'SSO sign-in is not enabled on this instance.'
    case 'provider-error':
      return 'The sign-in was cancelled or refused by the identity provider.'
    case 'provider-unreachable':
      return 'The identity provider could not be reached.'
    case 'invalid-state':
    case 'invalid-token':
      return 'This sign-in attempt has expired or was already used. Start again.'
    case 'not-linked':
      return 'This identity is not linked to a Buz account. Ask an administrator for access.'
    case 'not-initialized':
      return 'This instance has not been set up yet — create the first admin first.'
    case 'account-refused':
      return 'This account is not available for sign-in.'
    case 'provision-failed':
      return 'Creating your account failed — try again or contact an administrator.'
    case 'exchange-failed':
      return 'Sign-in could not be completed. Start again.'
    default:
      return 'Sign-in failed. Start again.'
  }
}
</script>

<template>
  <div class="flex min-h-screen items-center justify-center bg-background">
    <div class="w-full max-w-sm space-y-4 text-center">
      <div class="flex justify-center">
        <span
          class="flex h-10 w-10 items-center justify-center rounded-xl bg-primary text-sm font-semibold tracking-wide text-primary-foreground"
        >
          Bz
        </span>
      </div>
      <div v-if="status === 'exchanging'" class="space-y-1">
        <p class="text-sm font-medium">Completing sign-in…</p>
        <p class="type-meta">Finishing single sign-on</p>
      </div>
      <div v-else class="space-y-3">
        <p class="text-sm font-medium">Sign-in failed</p>
        <p class="type-meta">{{ error }}</p>
        <UiButton class="w-full" @click="router.push('/login')">Back to sign in</UiButton>
      </div>
    </div>
  </div>
</template>
