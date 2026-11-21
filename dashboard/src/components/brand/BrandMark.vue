<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useBrandingStore } from '@/stores/branding'

/**
 * The instance logo: the built-in "Bz" mark by default, the administrator's
 * upload when one exists. The default branch is intentionally byte-for-byte
 * the markup the four call sites used before branding existed, so a fresh
 * install looks exactly as it always did.
 */
const props = withDefaults(defineProps<{ size?: 'sm' | 'md' }>(), { size: 'sm' })

const branding = useBrandingStore()
const imageFailed = ref(false)

const boxClass = computed(() => (props.size === 'md' ? 'h-10 w-10' : 'h-8 w-8'))
const textClass = computed(() => (props.size === 'md' ? 'text-sm' : 'text-xs'))
const showCustom = computed(
  () => branding.hasCustom && !!branding.logoUrl && !imageFailed.value,
)

// A stale/broken upload (or a logo cleared on another node) falls back to the
// built-in mark instead of leaving a broken-image icon in the chrome.
watch(
  () => branding.logoUrl,
  () => {
    imageFailed.value = false
  },
)
</script>

<template>
  <img
    v-if="showCustom"
    :src="branding.logoUrl"
    alt="Logo"
    :class="['shrink-0 rounded-xl object-contain', boxClass]"
    @error="imageFailed = true"
  />
  <span
    v-else
    :class="[
      'flex shrink-0 items-center justify-center rounded-xl bg-primary font-semibold tracking-wide text-primary-foreground',
      boxClass,
      textClass,
    ]"
  >
    Bz
  </span>
</template>
