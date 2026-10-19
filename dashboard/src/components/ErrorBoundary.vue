<script setup lang="ts">
import { onErrorCaptured, ref, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import UiButton from '@/components/ui/UiButton.vue'

const props = defineProps<{ resetKey?: string | number }>()
const emit = defineEmits<{ error: [err: unknown] }>()

const route = useRoute()
const router = useRouter()
const error = ref<unknown>(null)

onErrorCaptured((err) => {
  console.error('[ErrorBoundary] captured render/lifecycle error:', err)
  error.value = err
  emit('error', err)
  // Stop propagation so a broken page cannot brick the shell / sidebar.
  return false
})

function reset() {
  error.value = null
}

watch(
  () => props.resetKey ?? route.fullPath,
  () => reset(),
)

function errorDetail(err: unknown): string {
  const msg = (err as Error)?.message || String(err)
  return msg.length > 200 ? `${msg.slice(0, 200)}…` : msg
}
</script>

<template>
  <div
    v-if="error"
    class="flex min-h-[50vh] flex-col items-center justify-center rounded-xl border border-destructive/30 bg-destructive/5 px-6 py-10 text-center text-sm"
  >
    <span aria-hidden="true" class="text-3xl">🛠️</span>
    <p class="mt-3 font-medium text-destructive">This page failed to render.</p>
    <p class="mt-1 max-w-md text-muted-foreground">
      The rest of Buz still works — the sidebar and other pages are unaffected.
      Retry the page, or head back to the Overview.
    </p>
    <p
      v-if="errorDetail(error)"
      class="mt-3 max-w-lg break-words rounded-lg bg-muted px-3 py-2 font-mono text-xs text-muted-foreground"
    >
      {{ errorDetail(error) }}
    </p>
    <div class="mt-5 flex items-center gap-3">
      <UiButton variant="outline" size="sm" @click="reset">Retry</UiButton>
      <UiButton size="sm" @click="router.replace({ name: 'dashboard' })">Go to Overview</UiButton>
    </div>
  </div>
  <slot v-else />
</template>
