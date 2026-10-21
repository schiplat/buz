<script setup lang="ts">
import { computed } from 'vue'
import { RouterView, useRoute, useRouter } from 'vue-router'
import { KeyRound, SlidersHorizontal, GitBranch } from '@lucide/vue'
import { cn } from '@/lib/utils'
import { useAuthStore } from '@/stores/auth'

const route = useRoute()
const router = useRouter()
const auth = useAuthStore()

type Tab = 'secrets' | 'variables' | 'github-sync'

const activeTab = computed<Tab>(() => {
  if (route.name === 'variables') return 'variables'
  if (route.name === 'github-sync') return 'github-sync'
  return 'secrets'
})

function go(tab: Tab) {
  if (activeTab.value === tab) return
  void router.push({ name: tab })
}
</script>

<template>
  <div class="flex flex-col gap-5">
    <!-- Tab bar -->
    <div class="inline-flex w-fit items-center gap-0.5 rounded-lg bg-muted/70 p-0.5">
      <button
        type="button"
        :class="
          cn(
            'inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-[13px] font-medium transition-all cursor-pointer',
            activeTab === 'secrets'
              ? 'bg-background text-foreground shadow-xs'
              : 'text-muted-foreground hover:text-foreground hover:bg-background/60',
          )
        "
        @click="go('secrets')"
      >
        <KeyRound class="h-3.5 w-3.5" />
        Secrets
      </button>
      <button
        type="button"
        :class="
          cn(
            'inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-[13px] font-medium transition-all cursor-pointer',
            activeTab === 'variables'
              ? 'bg-background text-foreground shadow-xs'
              : 'text-muted-foreground hover:text-foreground hover:bg-background/60',
          )
        "
        @click="go('variables')"
      >
        <SlidersHorizontal class="h-3.5 w-3.5" />
        Variables
      </button>
      <button
        v-if="auth.isPlatformAdmin"
        type="button"
        :class="
          cn(
            'inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-[13px] font-medium transition-all cursor-pointer',
            activeTab === 'github-sync'
              ? 'bg-background text-foreground shadow-xs'
              : 'text-muted-foreground hover:text-foreground hover:bg-background/60',
          )
        "
        @click="go('github-sync')"
      >
        <GitBranch class="h-3.5 w-3.5" />
        GitHub Sync
      </button>
    </div>

    <RouterView :key="route.name" />
  </div>
</template>
