<script setup lang="ts">
import { onMounted, onUnmounted } from 'vue'
import { RouterView, useRoute } from 'vue-router'
import AppSidebar from '@/components/layout/AppSidebar.vue'
import TopNav from '@/components/layout/TopNav.vue'
import ErrorBoundary from '@/components/ErrorBoundary.vue'
import ServerErrorBanner from '@/components/ServerErrorBanner.vue'
import ReviewGateBanner from '@/components/ReviewGateBanner.vue'
import BackToTop from '@/components/BackToTop.vue'
import { useUiStore } from '@/stores/ui'
import { useAuthStore } from '@/stores/auth'

const ui = useUiStore()
const auth = useAuthStore()
const route = useRoute()

function onKeydown(e: KeyboardEvent) {
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'b') {
    e.preventDefault()
    ui.toggleSidebar()
  }
}

onMounted(() => {
  window.addEventListener('keydown', onKeydown)
  void auth.fetchMe()
})
onUnmounted(() => window.removeEventListener('keydown', onKeydown))
</script>

<template>
  <div class="flex min-h-screen w-full bg-background">
    <AppSidebar />
    <div class="flex min-w-0 flex-1 flex-col">
      <TopNav />
      <main class="shell-inset flex-1 overflow-auto pb-8 pt-6">
        <div class="page-stack">
          <ReviewGateBanner />
          <ServerErrorBanner />
          <ErrorBoundary :reset-key="route.fullPath">
            <RouterView />
          </ErrorBoundary>
        </div>
      </main>
    </div>
    <BackToTop />
  </div>
</template>
