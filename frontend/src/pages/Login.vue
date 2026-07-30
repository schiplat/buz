<script setup lang="ts">
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { useAuthStore } from '@/stores/auth'
import UiButton from '@/components/ui/UiButton.vue'

const router = useRouter()
const authStore = useAuthStore()

const username = ref('')
const password = ref('')
const showPassword = ref(false)
const error = ref('')
const loading = ref(false)
const appVersion = __APP_VERSION__

async function handleLogin() {
  error.value = ''
  loading.value = true
  try {
    await authStore.login(username.value.trim(), password.value)
    router.push('/dashboard')
  } catch (e: any) {
    error.value = e.response?.data?.message || 'Login failed'
  } finally {
    loading.value = false
  }
}
</script>

<template>
  <div class="flex min-h-screen items-center justify-center bg-background">
    <div class="w-full max-w-sm space-y-6">
      <div class="text-center space-y-1">
        <div class="flex justify-center mb-3">
          <span class="flex h-10 w-10 items-center justify-center rounded-xl bg-primary text-sm font-semibold tracking-wide text-primary-foreground">
            Bz
          </span>
        </div>
        <h1 class="text-lg font-semibold tracking-tight">Buz</h1>
        <p class="type-meta">Sign in to your instance</p>
      </div>

      <form @submit.prevent="handleLogin" class="space-y-4">
        <div class="space-y-1.5">
          <label class="type-label">Username</label>
          <input
            v-model="username"
            type="text"
            autocomplete="username"
            placeholder="admin"
            class="flex h-10 w-full rounded-xl border border-border bg-card px-3 text-sm text-foreground placeholder:text-muted-foreground focus-visible:outline-none focus-visible:border-foreground/35 focus-visible:ring-2 focus-visible:ring-ring/30"
          />
        </div>
        <div class="space-y-1.5">
          <label class="type-label">Password</label>
          <div class="relative">
            <input
              v-model="password"
              :type="showPassword ? 'text' : 'password'"
              autocomplete="current-password"
              placeholder="Enter password"
              class="flex h-10 w-full rounded-xl border border-border bg-card px-3 pr-14 text-sm text-foreground placeholder:text-muted-foreground focus-visible:outline-none focus-visible:border-foreground/35 focus-visible:ring-2 focus-visible:ring-ring/30"
            />
            <button
              type="button"
              class="absolute right-2 top-1/2 -translate-y-1/2 text-[11px] text-muted-foreground hover:text-foreground transition-colors cursor-pointer"
              @click="showPassword = !showPassword"
            >
              {{ showPassword ? 'Hide' : 'Show' }}
            </button>
          </div>
        </div>
        <div v-if="error" class="field-error">{{ error }}</div>
        <UiButton type="submit" class="w-full" :disabled="loading">
          {{ loading ? 'Signing in…' : 'Sign in' }}
        </UiButton>
      </form>

      <p class="text-center font-mono text-[11px] text-muted-foreground/60">v{{ appVersion }}</p>
    </div>
  </div>
</template>
