<script setup lang="ts">
import UiButton from '@/components/ui/UiButton.vue'

// Landing for RP-initiated logout: the identity provider returns the browser
// here after ending the SSO session. Nothing to do but acknowledge.
</script>

<template>
  <div class="flex min-h-screen items-center justify-center bg-background px-4">
    <div class="w-full max-w-sm surface-card p-8 text-center">
      <h1 class="text-lg font-semibold tracking-tight">Signed out</h1>
      <p class="type-meta mt-2">
        You have been signed out of Buz and your identity provider session has
        ended.
      </p>
      <UiButton class="mt-6 w-full" @click="() => $router.push('/login')">
        Sign in again
      </UiButton>
    </div>
  </div>
</template>
