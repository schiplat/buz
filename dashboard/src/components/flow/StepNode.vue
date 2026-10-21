<script setup lang="ts">
import { computed } from 'vue'
import { Handle, Position } from '@vue-flow/core'
import { Loader2, CheckCircle2, XCircle, MinusCircle, Clock } from '@lucide/vue'
import { STEP_NODE_HEIGHT, STEP_NODE_WIDTH } from '@/components/flow/stepNodeSize'

export interface StepNodeData {
  stepIndex: number
  name: string
  execType: string
  status: string // 'success' | 'running' | 'failed' | 'cancelled' | 'pending'
  duration: string
  /** Focused step in Stage View */
  selected?: boolean
  /** Other steps when one is focused */
  dimmed?: boolean
}

const props = defineProps<{ data: StepNodeData }>()

const statusRing = computed(() => {
  if (props.data.selected) {
    switch (props.data.status) {
      case 'success': return 'ring-2 ring-emerald-500/50 border-emerald-500 shadow-md'
      case 'failed': return 'ring-2 ring-red-500/50 border-red-500 shadow-md'
      case 'running': return 'ring-2 ring-amber-500/50 border-amber-500 shadow-md'
      case 'cancelled': return 'ring-2 ring-slate-400/50 border-slate-400 shadow-md'
      default: return 'ring-2 ring-primary/40 border-primary shadow-md'
    }
  }
  switch (props.data.status) {
    case 'success': return 'ring-emerald-400/30 border-emerald-400/60'
    case 'failed': return 'ring-red-400/30 border-red-400/60'
    case 'running': return 'ring-amber-400/30 border-amber-400/60'
    case 'cancelled': return 'ring-slate-300/30 border-slate-300/60'
    default: return 'ring-slate-200/30 border-slate-200/60'
  }
})
</script>

<template>
  <div
    class="step-node relative box-border rounded-xl border bg-card shadow-xs transition-all overflow-hidden cursor-pointer hover:shadow-md"
    :class="[statusRing, data.dimmed && !data.selected ? 'opacity-40' : 'opacity-100']"
    :style="{ width: `${STEP_NODE_WIDTH}px`, height: `${STEP_NODE_HEIGHT}px` }"
    :title="data.name"
  >
    <div
      v-if="data.status === 'running'"
      class="h-1 w-full overflow-hidden shrink-0"
      style="background-color: #fbbf24; background-image: repeating-linear-gradient(-45deg, transparent, transparent 5px, rgba(255,255,255,0.3) 5px, rgba(255,255,255,0.3) 10px); background-size: 20px 20px; animation: stripe-flow 0.6s linear infinite;"
    />
    <div
      v-else
      class="h-1 w-full shrink-0"
      :class="{
        'bg-emerald-400': data.status === 'success',
        'bg-red-400': data.status === 'failed',
        'bg-slate-300': data.status === 'cancelled',
        'bg-slate-200': data.status === 'pending',
      }"
    />

    <div class="px-3 py-2 flex flex-col h-[calc(100%-4px)] min-h-0">
      <div class="flex items-start gap-2 min-h-0 flex-1">
        <div class="shrink-0 mt-0.5">
          <CheckCircle2 v-if="data.status === 'success'" class="h-4 w-4 text-emerald-500" />
          <XCircle v-else-if="data.status === 'failed'" class="h-4 w-4 text-red-500" />
          <Loader2 v-else-if="data.status === 'running'" class="h-4 w-4 text-amber-500 animate-spin" />
          <MinusCircle v-else-if="data.status === 'cancelled'" class="h-4 w-4 text-slate-400" />
          <span
            v-else
            class="inline-flex items-center justify-center h-4 w-4 rounded-full bg-slate-100 dark:bg-slate-800 text-[10px] font-bold text-slate-400 tabular-nums leading-none"
          >{{ String(data.stepIndex + 1).padStart(2, '0') }}</span>
        </div>
        <div class="min-w-0 flex-1 overflow-hidden">
          <div class="text-sm font-semibold text-foreground leading-snug truncate">{{ data.name }}</div>
          <span class="text-[10px] text-muted-foreground/70 uppercase tracking-wider mt-0.5 block truncate">{{ data.execType }}</span>
        </div>
      </div>

      <div class="flex items-center gap-1 text-xs shrink-0 pt-1">
        <Clock class="h-2.5 w-2.5 shrink-0 text-muted-foreground/50" />
        <span
          class="font-mono truncate"
          :class="data.status === 'running' ? 'text-amber-600' : 'text-muted-foreground'"
        >{{ data.duration }}</span>
      </div>
    </div>

    <Handle type="target" :position="Position.Left" class="w-2! h-2! border-none! bg-foreground/20!" />
    <Handle type="source" :position="Position.Right" class="w-2! h-2! border-none! bg-foreground/20!" />
  </div>
</template>
