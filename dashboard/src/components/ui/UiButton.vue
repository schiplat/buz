<script setup lang="ts">
import { buttonClass, type ButtonSize, type ButtonVariant } from './button'

withDefaults(
  defineProps<{
    variant?: ButtonVariant
    size?: ButtonSize
    type?: 'button' | 'submit' | 'reset'
    disabled?: boolean
    class?: string
    /** Render as this element (e.g. 'a' for link-styled buttons). */
    as?: string
    /** Target for link-styled buttons. */
    target?: string
    /** Href for link-styled buttons. */
    href?: string
  }>(),
  { variant: 'default', size: 'default', type: 'button', disabled: false, as: 'button' },
)
</script>

<template>
  <component
    :is="as"
    :type="as === 'button' ? type : undefined"
    :disabled="as === 'button' ? disabled : undefined"
    :href="as === 'a' ? href : undefined"
    :target="as === 'a' ? target : undefined"
    :class="buttonClass(variant, size, $props.class)"
  >
    <slot />
  </component>
</template>
