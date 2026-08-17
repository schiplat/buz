<script setup lang="ts">
import { computed } from 'vue'
import { cn } from '@/lib/utils'

const props = defineProps<{
  status: string
}>()

const tone = computed(() => {
  switch (props.status) {
    case 'online'    : return 'bg-success/12 text-success'
    case 'running'   : return 'bg-info/12 text-info'
    case 'offline'   : return 'bg-muted text-muted-foreground'
    case 'busy'      : return 'bg-warning/12 text-warning'
    case 'success'   : return 'bg-success/12 text-success'
    case 'failed'    : return 'bg-destructive/12 text-destructive'
    case 'cancelled' : return 'bg-muted text-muted-foreground'
    case 'timeout'   : return 'bg-warning/12 text-warning'
    case 'pending'   : return 'bg-muted text-muted-foreground'
    default: return 'bg-muted text-muted-foreground'
  }
})
</script>

<template>
  <span :class="cn('inline-flex items-center rounded-full px-2 py-0.5 text-xs font-medium uppercase tracking-wide', tone)">
    {{ status }}
  </span>
</template>
