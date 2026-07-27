<script setup lang="ts">
import { buttonClass, type ButtonSize, type ButtonVariant } from './button'

withDefaults(
  defineProps<{
    variant?: ButtonVariant
    size?: ButtonSize
    type?: 'button' | 'submit' | 'reset'
    disabled?: boolean
    class?: string
  }>(),
  { variant: 'default', size: 'default', type: 'button', disabled: false },
)
</script>

<template>
  <button
    :type="type"
    :disabled="disabled"
    :class="buttonClass(variant, size, $props.class)"
  >
    <slot />
  </button>
</template>
