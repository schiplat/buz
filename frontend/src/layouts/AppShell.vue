<script setup lang="ts">
import { onMounted, onUnmounted } from 'vue'
import AppSidebar from '@/components/layout/AppSidebar.vue'
import TopNav from '@/components/layout/TopNav.vue'
import { useUiStore } from '@/stores/ui'

const ui = useUiStore()

function onKeydown(e: KeyboardEvent) {
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'b') {
    e.preventDefault()
    ui.toggleSidebar()
  }
}

onMounted(() => window.addEventListener('keydown', onKeydown))
onUnmounted(() => window.removeEventListener('keydown', onKeydown))
</script>

<template>
  <div class="flex min-h-screen w-full bg-background">
    <AppSidebar />
    <div class="flex min-w-0 flex-1 flex-col">
      <TopNav />
      <main class="shell-inset flex-1 overflow-auto pb-8 pt-6">
        <div class="page-stack">
          <RouterView />
        </div>
      </main>
    </div>
  </div>
</template>
