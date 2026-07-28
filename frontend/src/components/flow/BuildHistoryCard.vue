<script setup lang="ts">
import { ref, computed, nextTick } from 'vue'
import { useRouter } from 'vue-router'
import { ChevronDown, ChevronRight, ArrowRight } from '@lucide/vue'
import { parseUtc } from '@/utils/time'
import type { Job, LogChunk } from '@/api/jobs'
import StepLogPanel from '@/components/flow/StepLogPanel.vue'

const props = defineProps<{
  job: Job
  runNumber: number
  logs: LogChunk[] // per-job logs, passed from parent
}>()

const router = useRouter()
const expanded = ref(false)
const expandedStep = ref<number | null>(null)

const tasks = computed(() => props.job.snapshot?.tasks || [])
const stepStatuses = computed(() => (props.job.step_statuses || []) as any[])

function getStepStatus(stepOrder: number): string {
  const found = stepStatuses.value.find((s: any) => Number(s.step_order) === stepOrder)
  return found?.status || 'pending'
}

function fmtStepDuration(stepOrder: number): string {
  const found = stepStatuses.value.find((s: any) => Number(s.step_order) === stepOrder)
  if (!found?.started_at) return '\u2014'
  const a = parseUtc(found.started_at)
  const b = found.finished_at ? parseUtc(found.finished_at) : Date.now()
  const sec = Math.max(0, Math.floor((b - a) / 1000))
  if (sec < 60) return `${sec}s`
  if (sec < 3600) return `${Math.floor(sec / 60)}m ${sec % 60}s`
  return `${Math.floor(sec / 3600)}h ${Math.floor((sec % 3600) / 60)}m`
}

function fmtDuration(start: string | null, end: string | null) {
  if (!start) return '\u2014'
  const a = parseUtc(start)
  const b = end ? parseUtc(end) : Date.now()
  const s = Math.max(0, Math.floor((b - a) / 1000))
  if (s < 60) return `${s}s`
  if (s < 3600) return `${Math.floor(s / 60)}m ${s % 60}s`
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`
}

function fmtTime(ts: string | null | undefined) {
  if (!ts) return ''
  return ts.trim().replace('T', ' ').slice(0, 19)
}

const emit = defineEmits<{
  (e: 'expand', jobId: string): void
}>()

function toggle() {
  expanded.value = !expanded.value
  if (expanded.value) {
    emit('expand', props.job.id)
    expandedStep.value = null
  } else {
    expandedStep.value = null
  }
}

function toggleStepLog(stepOrder: number) {
  expandedStep.value = expandedStep.value === stepOrder ? null : stepOrder
  nextTick(() => {
    // Re-fit terminal if panel exists
    const el = document.getElementById(`hist-term-${props.job.id}-${stepOrder}`)
    if (el) { /* StepLogPanel handles fit internally */ }
  })
}

function goToStep(stepOrder: number) {
  router.push(`/jobs/${props.job.id}?step=${stepOrder}`)
}
</script>

<template>
  <div class="rounded-xl border border-border/50 bg-card shadow-sm overflow-hidden">
    <!-- Run header bar -->
    <div
      class="flex items-center gap-3 px-4 py-3 cursor-pointer hover:bg-muted/20 transition-colors"
      :class="job.status === 'running' ? 'bg-amber-500/[0.04]' : job.status === 'failed' ? 'bg-red-500/[0.03]' : ''"
      @click="toggle"
    >
      <ChevronDown v-if="expanded" class="h-3.5 w-3.5 text-muted-foreground shrink-0" />
      <ChevronRight v-else class="h-3.5 w-3.5 text-muted-foreground shrink-0" />

      <span class="font-mono text-[13px] font-bold tabular-nums text-foreground shrink-0">#{{ runNumber }}</span>

      <span
        class="inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[10px] font-semibold shrink-0"
        :class="job.status === 'success'
          ? 'bg-emerald-500/10 text-emerald-600'
          : job.status === 'failed'
            ? 'bg-red-500/10 text-red-500'
            : job.status === 'running'
              ? 'bg-amber-500/10 text-amber-600'
              : job.status === 'cancelled'
                ? 'bg-slate-500/10 text-slate-500'
                : 'bg-slate-500/10 text-slate-500'"
      >
        <span class="w-1.5 h-1.5 rounded-full shrink-0"
          :class="job.status === 'success'
            ? 'bg-emerald-500'
            : job.status === 'failed'
              ? 'bg-red-500'
              : job.status === 'running'
                ? 'bg-amber-400 animate-pulse'
                : 'bg-slate-400'" />
        {{ job.status }}
      </span>

      <span class="font-mono text-[11px] tabular-nums text-muted-foreground shrink-0">{{ fmtDuration(job.start_time, job.end_time) }}</span>

      <span class="flex-1" />

      <span class="font-mono text-[11px] text-muted-foreground/70 shrink-0">{{ fmtTime(job.start_time || job.created_at) }}</span>

      <ArrowRight class="h-3 w-3 text-muted-foreground/30 shrink-0" />
    </div>

    <!-- Step chips row -->
    <div class="px-4 pb-3">
      <div class="flex flex-wrap items-center gap-1">
        <template v-for="(task, si) in tasks" :key="task.step_order">
          <span v-if="Number(si) > 0" class="text-muted-foreground/25 shrink-0 text-[10px]">→</span>
          <div
            class="inline-flex items-center justify-center w-7 h-7 rounded-md text-[10px] font-bold tabular-nums transition-all shrink-0"
            :class="[
              getStepStatus(task.step_order) === 'pending'
                ? 'bg-transparent text-slate-400 border border-slate-200'
                : getStepStatus(task.step_order) === 'success'
                  ? 'bg-emerald-500/10 text-emerald-600 border border-emerald-400/40 cursor-pointer hover:bg-emerald-500/20'
                  : getStepStatus(task.step_order) === 'failed'
                    ? 'bg-red-500/10 text-red-500 border border-red-400/40 cursor-pointer hover:bg-red-500/20'
                    : getStepStatus(task.step_order) === 'running'
                      ? 'bg-amber-500/10 text-amber-600 border border-amber-400/40 ring-1 ring-amber-400/20 cursor-pointer'
                      : 'bg-slate-500/10 text-slate-500 border border-slate-300/50 cursor-pointer',
            ]"
            :title="`#${task.step_order + 1} ${task.name} (${fmtStepDuration(task.step_order)})`"
            @click.stop="getStepStatus(task.step_order) !== 'pending' && goToStep(task.step_order)"
          >
            {{ task.step_order + 1 }}
          </div>
        </template>
      </div>
    </div>

    <!-- Expanded detail: per-step log terminals -->
    <div v-if="expanded" class="border-t border-border/40 bg-muted/10">
      <!-- Step selector tabs -->
      <div class="flex items-center gap-1 px-4 pt-3 pb-2 overflow-x-auto">
        <button
          v-for="task in tasks"
          :key="task.step_order"
          class="flex items-center gap-1.5 rounded-md px-2.5 py-1.5 text-[11px] font-medium transition-colors shrink-0"
          :class="expandedStep === task.step_order
            ? 'bg-background text-foreground shadow-sm'
            : 'text-muted-foreground hover:text-foreground hover:bg-background/50'"
          @click="toggleStepLog(task.step_order)"
        >
          <span class="w-1.5 h-1.5 rounded-full"
            :class="getStepStatus(task.step_order) === 'success'
              ? 'bg-emerald-500'
              : getStepStatus(task.step_order) === 'failed'
                ? 'bg-red-500'
                : getStepStatus(task.step_order) === 'running'
                  ? 'bg-amber-400 animate-pulse'
                  : 'bg-slate-300'" />
          {{ task.name }}
        </button>
      </div>

      <!-- Selected step log terminal -->
      <div v-if="expandedStep != null" class="px-4 pb-4">
        <div class="rounded-lg border border-border/60 overflow-hidden" style="height: 280px">
          <StepLogPanel
            :logs="logs.filter(l => l.step_order === expandedStep)"
            :step-order="expandedStep"
            :key="expandedStep"
          />
        </div>
        <div
          v-if="logs.filter(l => l.step_order === expandedStep).length === 0"
          class="text-[10px] text-center text-muted-foreground/50 mt-1"
        >No log records for this step</div>
      </div>
      <div v-else class="px-4 pb-3 text-[11px] text-muted-foreground">
        Select a step above to view its logs
      </div>
    </div>
  </div>
</template>
