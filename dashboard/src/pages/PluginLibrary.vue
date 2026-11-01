<script setup lang="ts">
import { computed } from 'vue'
import { RouterView, useRoute, useRouter } from 'vue-router'
import { Package, CloudDownload, SlidersHorizontal } from '@lucide/vue'
import { cn } from '@/lib/utils'

const route = useRoute()
const router = useRouter()

type Tab = 'plugin-pool' | 'plugin-sources'

const activeTab = computed<Tab>(() => {
  if (route.name === 'plugin-sources') return 'plugin-sources'
  return 'plugin-pool'
})

function go(tab: Tab) {
  if (activeTab.value === tab) return
  void router.push({ name: tab })
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
            activeTab === 'plugin-pool'
              ? 'bg-background text-foreground shadow-xs'
              : 'text-muted-foreground hover:text-foreground hover:bg-background/60',
          )
        "
        @click="go('plugin-pool')"
      >
        <Package class="h-3.5 w-3.5" />
        Plugins
      </button>
      <button
        type="button"
        :class="
          cn(
            'inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-[13px] font-medium transition-all cursor-pointer',
            activeTab === 'plugin-sources'
              ? 'bg-background text-foreground shadow-xs'
              : 'text-muted-foreground hover:text-foreground hover:bg-background/60',
          )
        "
        @click="go('plugin-sources')"
      >
        <CloudDownload class="h-3.5 w-3.5" />
        Sources &amp; Settings
      </button>
    </div>

    <RouterView />
  </div>
</template>
