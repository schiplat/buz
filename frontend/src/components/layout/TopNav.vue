<script setup lang="ts">
import { PanelLeft, User } from '@lucide/vue'
import { computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import UiButton from '@/components/ui/UiButton.vue'
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

function handleLogout() {
  auth.logout()
  router.push({ name: 'login' })
}
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
    <div class="flex items-center gap-1.5">
      <div v-if="auth.isAuthenticated" class="flex items-center gap-2">
        <button
          type="button"
          class="inline-flex items-center gap-2 rounded-full py-1 pl-1.5 pr-3 text-sm transition-colors hover:bg-muted"
          @click="handleLogout"
          title="Sign out"
        >
          <span class="flex h-7 w-7 items-center justify-center rounded-full bg-secondary text-secondary-foreground text-xs font-semibold uppercase">
            {{ (auth.username || '?').slice(0, 1) }}
          </span>
          <span class="text-sm text-muted-foreground">{{ auth.username }}</span>
          <User class="h-3.5 w-3.5 text-muted-foreground" />
        </button>
      </div>
    </div>
  </header>
</template>
