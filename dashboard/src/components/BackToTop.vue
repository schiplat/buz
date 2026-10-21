<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue'
import { ArrowUp } from '@lucide/vue'

const visible = ref(false)

// The app scrolls on `window` (the AppShell wrapper is `min-h-screen`, so the
// page grows and the browser scrolls document/window — `<main>` never scrolls).
// Listening to the window is the only reliable signal.
function onScroll() {
  visible.value = window.scrollY > 240
}

function scrollToTop() {
  window.scrollTo({ top: 0, behavior: 'smooth' })
}

onMounted(() => {
  window.addEventListener('scroll', onScroll, { passive: true })
  onScroll()
})
onUnmounted(() => window.removeEventListener('scroll', onScroll))
</script>

<template>
  <button
    v-if="visible"
    aria-label="Back to top"
    title="Back to top"
    class="fixed bottom-6 right-6 z-50 flex h-10 w-10 items-center justify-center rounded-full border border-border/60 bg-card/90 text-muted-foreground shadow-lg backdrop-blur-sm transition-all hover:-translate-y-0.5 hover:bg-accent hover:text-foreground"
    @click="scrollToTop"
  >
    <ArrowUp class="h-4.5 w-4.5" />
  </button>
</template>
