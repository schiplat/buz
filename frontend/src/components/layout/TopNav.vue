<script setup lang="ts">
import { ChevronDown, KeyRound, LogOut, PanelLeft, User } from '@lucide/vue'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import UiButton from '@/components/ui/UiButton.vue'
import { authApi } from '@/api/auth'
import { useAuthStore } from '@/stores/auth'
import { useUiStore } from '@/stores/ui'

const ui = useUiStore()
const auth = useAuthStore()
const route = useRoute()
const router = useRouter()

const pageTitle = computed(() => {
  const matched = route.matched[route.matched.length - 1]
  return (matched?.meta?.title as string) || 'Buz'
})

const pageSubtitle = 'Lightweight Cross-Network Orchestration'

const menuOpen = ref(false)
const menuRoot = ref<HTMLElement | null>(null)

const showPasswordDialog = ref(false)
const currentPassword = ref('')
const newPassword = ref('')
const confirmPassword = ref('')
const passwordError = ref('')
const passwordSaving = ref(false)

function handleLogout() {
  menuOpen.value = false
  auth.logout()
  router.push({ name: 'login' })
}

function openChangePassword() {
  menuOpen.value = false
  currentPassword.value = ''
  newPassword.value = ''
  confirmPassword.value = ''
  passwordError.value = ''
  showPasswordDialog.value = true
}

function closePasswordDialog() {
  if (passwordSaving.value) return
  showPasswordDialog.value = false
}

async function submitChangePassword() {
  passwordError.value = ''
  if (!currentPassword.value) {
    passwordError.value = 'Current password is required'
    return
  }
  if (!newPassword.value) {
    passwordError.value = 'New password is required'
    return
  }
  if (newPassword.value.length < 6) {
    passwordError.value = 'New password must be at least 6 characters'
    return
  }
  if (newPassword.value !== confirmPassword.value) {
    passwordError.value = 'New password and confirmation do not match'
    return
  }
  if (newPassword.value === currentPassword.value) {
    passwordError.value = 'New password must differ from current password'
    return
  }

  passwordSaving.value = true
  try {
    await authApi.changePassword(currentPassword.value, newPassword.value)
    showPasswordDialog.value = false
  } catch (e: any) {
    passwordError.value =
      e.response?.data?.message || e.message || 'Failed to change password'
  } finally {
    passwordSaving.value = false
  }
}

function onDocClick(e: MouseEvent) {
  if (!menuOpen.value) return
  const el = menuRoot.value
  if (el && !el.contains(e.target as Node)) {
    menuOpen.value = false
  }
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    menuOpen.value = false
    if (showPasswordDialog.value && !passwordSaving.value) {
      showPasswordDialog.value = false
    }
  }
}

onMounted(() => {
  document.addEventListener('click', onDocClick)
  document.addEventListener('keydown', onKeydown)
})
onUnmounted(() => {
  document.removeEventListener('click', onDocClick)
  document.removeEventListener('keydown', onKeydown)
})
</script>

<template>
  <header
    class="shell-inset sticky top-0 z-10 flex h-14 items-center justify-between gap-4 bg-background/90 backdrop-blur-md"
  >
    <div class="flex min-w-0 items-center gap-3">
      <UiButton
        v-if="ui.isCollapsed"
        variant="ghost"
        size="icon"
        title="Expand sidebar (⌘B)"
        @click="ui.toggleSidebar()"
      >
        <PanelLeft class="h-4 w-4" />
      </UiButton>
      <div class="min-w-0">
        <p class="truncate text-sm font-medium tracking-tight">{{ pageTitle }}</p>
        <p class="type-meta truncate">{{ pageSubtitle }}</p>
      </div>
    </div>

    <div v-if="auth.isAuthenticated" ref="menuRoot" class="relative">
      <button
        type="button"
        class="inline-flex items-center gap-2 rounded-full py-1 pl-1.5 pr-2.5 text-sm transition-colors hover:bg-muted"
        :aria-expanded="menuOpen"
        aria-haspopup="menu"
        @click.stop="menuOpen = !menuOpen"
      >
        <span class="flex h-7 w-7 items-center justify-center rounded-full bg-secondary text-secondary-foreground text-xs font-semibold uppercase">
          {{ (auth.username || '?').slice(0, 1) }}
        </span>
        <span class="text-sm text-muted-foreground max-w-[120px] truncate">{{ auth.username }}</span>
        <ChevronDown
          class="h-3.5 w-3.5 text-muted-foreground transition-transform"
          :class="menuOpen && 'rotate-180'"
        />
      </button>

      <div
        v-if="menuOpen"
        role="menu"
        class="absolute right-0 top-full z-50 mt-1.5 w-44 overflow-hidden rounded-xl border border-border bg-background py-1 shadow-lg"
      >
        <button
          type="button"
          role="menuitem"
          class="flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-foreground hover:bg-muted"
          @click="openChangePassword"
        >
          <KeyRound class="h-3.5 w-3.5 text-muted-foreground" />
          Change password
        </button>
        <button
          type="button"
          role="menuitem"
          class="flex w-full items-center gap-2 px-3 py-2 text-left text-sm text-foreground hover:bg-muted"
          @click="handleLogout"
        >
          <LogOut class="h-3.5 w-3.5 text-muted-foreground" />
          Log out
        </button>
      </div>
    </div>
  </header>

  <Teleport to="body">
    <div
      v-if="showPasswordDialog"
      class="fixed inset-0 z-50 flex items-center justify-center"
    >
      <div class="absolute inset-0 bg-black/50" @click="closePasswordDialog" />
      <div class="relative z-10 w-full max-w-sm rounded-xl border border-border bg-background p-6 shadow-2xl">
        <div class="mb-4 flex items-center gap-2">
          <User class="h-4 w-4 text-muted-foreground" />
          <h2 class="text-base font-semibold">Change password</h2>
        </div>
        <form class="space-y-3.5" @submit.prevent="submitChangePassword">
          <div>
            <label class="type-label block mb-1.5">
              Current password <span class="text-red-500">*</span>
            </label>
            <input
              v-model="currentPassword"
              type="password"
              autocomplete="current-password"
              class="flex h-10 w-full rounded-xl border border-border bg-card px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/30"
              placeholder="Enter current password"
            />
          </div>
          <div>
            <label class="type-label block mb-1.5">
              New password <span class="text-red-500">*</span>
            </label>
            <input
              v-model="newPassword"
              type="password"
              autocomplete="new-password"
              class="flex h-10 w-full rounded-xl border border-border bg-card px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/30"
              placeholder="At least 6 characters"
            />
          </div>
          <div>
            <label class="type-label block mb-1.5">
              Confirm new password <span class="text-red-500">*</span>
            </label>
            <input
              v-model="confirmPassword"
              type="password"
              autocomplete="new-password"
              class="flex h-10 w-full rounded-xl border border-border bg-card px-3 text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/30"
              placeholder="Re-enter new password"
            />
          </div>
          <p v-if="passwordError" class="field-error">{{ passwordError }}</p>
          <div class="flex justify-end gap-2 pt-1">
            <UiButton type="button" variant="ghost" :disabled="passwordSaving" @click="closePasswordDialog">
              Cancel
            </UiButton>
            <UiButton type="submit" :disabled="passwordSaving">
              {{ passwordSaving ? 'Saving…' : 'Update password' }}
            </UiButton>
          </div>
        </form>
      </div>
    </div>
  </Teleport>
</template>
