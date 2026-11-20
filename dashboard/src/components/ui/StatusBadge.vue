<script setup lang="ts">
import { computed } from 'vue'
import { cn } from '@/lib/utils'

const props = defineProps<{
  status: string
}>()

const tone = computed(() => {
  switch (props.status) {
    case 'online'    : return 'chip-success'
    case 'running'   : return 'chip-info'
    case 'offline'   : return 'chip-neutral'
    case 'busy'      : return 'chip-warning'
    case 'success'   : return 'chip-success'
    case 'failed'    : return 'chip-danger'
    case 'cancelled' : return 'chip-neutral'
    case 'timeout'   : return 'chip-warning'
    case 'pending'   : return 'chip-neutral'
    case 'queued'    : return 'chip-info'
    case 'waiting_approval': return 'chip-warning'
    case 'insync'    : return 'chip-success'
    case 'syncing'   : return 'chip-info'
    case 'degraded'  : return 'chip-danger'
    default: return 'chip-neutral'
  }
})
</script>

<template>
  <span :class="cn(tone)">
    {{ status }}
  </span>
</template>
