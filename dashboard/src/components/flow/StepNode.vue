<script setup lang="ts">
import { computed } from 'vue'
import { Handle, Position } from '@vue-flow/core'
import { Terminal, Server, Globe, Box, Loader2, CheckCircle2, XCircle, MinusCircle, Clock } from '@lucide/vue'

export interface StepNodeData {
  stepIndex: number
  name: string
  execType: string
  status: string // 'success' | 'running' | 'failed' | 'cancelled' | 'pending'
  duration: string
  onClick?: () => void
}

const props = defineProps<{ data: StepNodeData }>()

const clickable = computed(() => props.data.status !== 'pending')

const statusRing = computed(() => {
  switch (props.data.status) {
    case 'success': return 'ring-emerald-400/30 border-emerald-400/60'
    case 'failed':  return 'ring-red-400/30 border-red-400/60'
    case 'running': return 'ring-amber-400/30 border-amber-400/60'
    case 'cancelled': return 'ring-slate-300/30 border-slate-300/60'
    default:         return 'ring-slate-200/30 border-slate-200/60'
  }
})

const iconComponent = computed(() => {
  const name = props.data.name.toLowerCase()
  if (name.includes('ssh') || name.includes('remote')) return Server
  if (name.includes('deploy') || name.includes('publish')) return Globe
  if (name.includes('shell') || name.includes('cmd')) return Terminal
  return Box
})

function handleClick() {
  if (clickable.value) {
    props.data.onClick?.()
  }
}
</script>

<template>
  <div
    class="step-node relative min-w-[152px] max-w-[180px] rounded-xl border bg-card shadow-sm transition-all overflow-hidden"
    :class="[statusRing, clickable ? 'cursor-pointer hover:shadow-md' : 'cursor-not-allowed opacity-70']"
    @click="handleClick"
  >
    <!-- Animated stripe bar for running -->
    <div
      v-if="data.status === 'running'"
      class="h-1 w-full overflow-hidden"
      style="background-color: #fbbf24; background-image: repeating-linear-gradient(-45deg, transparent, transparent 5px, rgba(255,255,255,0.3) 5px, rgba(255,255,255,0.3) 10px); background-size: 20px 20px; animation: stripe-flow 0.6s linear infinite;"
    />
    <div
      v-else
      class="h-1 w-full"
      :class="{
        'bg-emerald-400': data.status === 'success',
        'bg-red-400': data.status === 'failed',
        'bg-slate-300': data.status === 'cancelled',
        'bg-slate-200': data.status === 'pending',
      }"
    />

    <div class="px-3 py-2.5 space-y-2">
      <!-- Status icon + name -->
      <div class="flex items-start gap-2">
        <div class="shrink-0 mt-0.5">
          <CheckCircle2 v-if="data.status === 'success'" class="h-4 w-4 text-emerald-500" />
          <XCircle v-else-if="data.status === 'failed'" class="h-4 w-4 text-red-500" />
          <Loader2 v-else-if="data.status === 'running'" class="h-4 w-4 text-amber-500 animate-spin" />
          <MinusCircle v-else-if="data.status === 'cancelled'" class="h-4 w-4 text-slate-400" />
          <span v-else class="inline-flex items-center justify-center h-4 w-4 rounded-full bg-slate-100 dark:bg-slate-800 text-xs font-bold text-slate-400 tabular-nums leading-none">{{ String(data.stepIndex + 1).padStart(2, '0') }}</span>
        </div>
        <div class="min-w-0 flex-1">
          <div class="text-sm font-semibold text-foreground leading-snug break-words">{{ data.name }}</div>
          <span class="text-xs text-muted-foreground/70 uppercase tracking-wider mt-0.5 block">{{ data.execType }}</span>
        </div>
      </div>

      <!-- Duration -->
      <div class="flex items-center gap-1 text-xs">
        <Clock class="h-2.5 w-2.5 shrink-0 text-muted-foreground/50" />
        <span
          class="font-mono"
          :class="data.status === 'running' ? 'text-amber-600' : 'text-muted-foreground'"
        >{{ data.duration }}</span>
      </div>
    </div>

    <Handle type="target" :position="Position.Left" class="!w-2 !h-2 !border-none !bg-foreground/20" />
    <Handle type="source" :position="Position.Right" class="!w-2 !h-2 !border-none !bg-foreground/20" />
  </div>
</template>
