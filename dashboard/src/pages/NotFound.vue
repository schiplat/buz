<script setup lang="ts">
import { useRouter } from 'vue-router'
import UiButton from '@/components/ui/UiButton.vue'

const router = useRouter()

function goBack() {
  if (window.history.length > 1) {
    router.back()
  } else {
    void router.replace({ name: 'dashboard' })
  }
}
</script>

<template>
  <div class="flex min-h-[60vh] flex-col items-center justify-center text-center">
    <p class="text-6xl font-semibold tracking-tight text-muted-foreground/40">404</p>
    <h1 class="mt-4 text-xl font-semibold">Page not found</h1>
    <p class="mt-2 max-w-md text-sm text-muted-foreground">
      The page you are looking for does not exist or has moved.
      Check the address, or pick one of the sections in the sidebar.
    </p>
    <div class="mt-6 flex items-center gap-3">
      <UiButton variant="outline" @click="goBack">Go back</UiButton>
      <UiButton @click="router.replace({ name: 'dashboard' })">Go to Overview</UiButton>
    </div>
  </div>
</template>
