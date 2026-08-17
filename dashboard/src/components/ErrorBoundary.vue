<script setup lang="ts">
import { onErrorCaptured, ref, watch } from 'vue'
import { useRoute } from 'vue-router'

const props = defineProps<{ resetKey?: string | number }>()
const emit = defineEmits<{ error: [err: unknown] }>()

const route = useRoute()
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
</script>

<template>
  <div v-if="error" class="rounded-xl border border-destructive/30 bg-destructive/5 p-6 text-sm">
    <p class="font-medium text-destructive">This page failed to render.</p>
    <p class="mt-1 text-muted-foreground">
      The sidebar should still work. Try another page, or reload.
    </p>
    <button
      type="button"
      class="mt-3 rounded-lg border border-border px-3 py-1.5 text-xs hover:bg-muted"
      @click="reset"
    >
      Retry
    </button>
  </div>
  <slot v-else />
</template>
