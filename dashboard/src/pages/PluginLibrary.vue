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
    <div>
      <h1 class="text-lg font-semibold tracking-tight">Plugin library</h1>
      <p class="mt-0.5 text-sm text-muted-foreground">
        Templates pipeline authors may reference with
        <code class="font-mono text-[13px]">uses:</code>. Official and community sources share the
        same pipeline — curating the pool is the only gate.
      </p>
    </div>

    <!-- Tab bar -->
    <div class="inline-flex w-fit items-center gap-0.5 rounded-lg bg-muted/70 p-0.5">
    </div>

    <RouterView />
  </div>
</template>
