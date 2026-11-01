<script setup lang="ts">
import { computed } from 'vue'
import { RouterView, useRoute, useRouter } from 'vue-router'
import { Monitor, Boxes } from '@lucide/vue'
import { cn } from '@/lib/utils'

const route = useRoute()
const router = useRouter()

type Tab = 'runners' | 'sandbox-hosts'

const activeTab = computed<Tab>(() => {
  if (route.name === 'sandbox-hosts') return 'sandbox-hosts'
  return 'runners'
})

function go(tab: Tab) {
  if (activeTab.value === tab) return
  // Runners content lives at /runners/list — never fall back to the bare
  // /runners path, which only redirects.
  void router.push({ name: tab === 'runners' ? 'runners-list' : tab })
}
</script>

<template>
  <div class="flex flex-col gap-5">
    <!-- Tab bar (page title lives in the TopNav; tabs stay the first row) -->
    <div class="inline-flex w-fit items-center gap-0.5 rounded-lg bg-muted/70 p-0.5">
      <button
        type="button"
        :class="
          cn(
            'inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-[13px] font-medium transition-all cursor-pointer',
            activeTab === 'runners'
              ? 'bg-background text-foreground shadow-xs'
              : 'text-muted-foreground hover:text-foreground hover:bg-background/60',
          )
        "
        @click="go('runners')"
      >
        <Monitor class="h-3.5 w-3.5" />
        Runners
      </button>
      <button
        type="button"
        :class="
          cn(
            'inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-[13px] font-medium transition-all cursor-pointer',
            activeTab === 'sandbox-hosts'
              ? 'bg-background text-foreground shadow-xs'
              : 'text-muted-foreground hover:text-foreground hover:bg-background/60',
          )
        "
        @click="go('sandbox-hosts')"
      >
        <Boxes class="h-3.5 w-3.5" />
        Sandbox hosts
      </button>
    </div>

    <RouterView />
  </div>
</template>
