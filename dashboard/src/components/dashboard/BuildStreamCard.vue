<script setup lang="ts">
import { computed } from 'vue'
import { useRouter } from 'vue-router'
import { ArrowRight, Clock } from '@lucide/vue'
import type { RecentJob } from '@/api/jobs'
import { parseUtc } from '@/utils/time'

const props = defineProps<{
  jobs: RecentJob[]
  loading: boolean
}>()

const router = useRouter()

const TRIGGER_LABELS: Record<string, string> = {
  manual: 'Manual',
  webhook: 'Webhook',
  cron: 'Cron',
  git: 'Git',
  api: 'API',
}

function triggerLabel(triggerBy: string): string {
  return TRIGGER_LABELS[triggerBy] ?? (triggerBy || '—')
}

function statusDotClass(status: string): string {
  switch (status) {
    case 'success':
      return 'bg-emerald-500'
    case 'failed':
      return 'bg-red-500'
    case 'running':
      return 'bg-amber-400 animate-pulse'
    case 'cancelled':
      return 'bg-slate-400'
    default:
      return 'bg-slate-300'
  }
}

function timeAgo(ts: string | null | undefined): string {
  if (!ts) return '—'
  const ms = Date.now() - parseUtc(ts)
  const sec = Math.max(0, Math.floor(ms / 1000))
  if (sec < 60) return `${sec}s ago`
  const min = Math.floor(sec / 60)
  if (min < 60) return `${min}m ago`
  const hr = Math.floor(min / 60)
  if (hr < 24) return `${hr}h ago`
  const day = Math.floor(hr / 24)
  return day === 1 ? 'yesterday' : `${day}d ago`
}

const empty = computed(() => !props.loading && props.jobs.length === 0)
</script>

<template>
  <div class="surface-card flex flex-col">
    <div class="flex items-center justify-between gap-2 px-5 pt-5">
      <div class="flex items-center gap-2">
        <Clock class="h-3.5 w-3.5 text-muted-foreground" />
        <p class="type-eyebrow">Build stream</p>
      </div>
      <a
        href="/jobs"
        class="inline-flex items-center gap-1 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground"
        @click.prevent="router.push('/jobs')"
      >
        View all
        <ArrowRight class="h-3 w-3" />
      </a>
    </div>

    <div class="mt-3 px-2 pb-2">
      <div v-if="loading" class="space-y-1 px-3 py-1">
        <div v-for="n in 6" :key="n" class="animate-pulse">
          <div class="h-4 w-3/4 rounded-sm bg-muted" />
          <div class="mt-1.5 h-3 w-1/2 rounded-sm bg-muted/70" />
        </div>
      </div>

      <div v-else-if="empty" class="px-3 py-8 text-center text-sm text-muted-foreground">
        No recent builds
      </div>

      <ul v-else class="scrollbar-sidebar max-h-[560px] space-y-0.5 overflow-y-auto">
        <li v-for="j in jobs" :key="j.id">
          <a
            :href="`/jobs/${j.id}`"
            class="group flex items-start gap-2.5 rounded-xl px-3 py-2.5 transition-colors hover:bg-muted/70"
            @click.prevent="router.push(`/jobs/${j.id}`)"
          >
            <span class="mt-1.5 h-2 w-2 shrink-0 rounded-full" :class="statusDotClass(j.status)" />
            <span class="min-w-0 flex-1">
              <span class="block truncate font-mono text-[12.5px] font-medium leading-snug text-foreground/90">
                {{ j.pipeline_name || j.pipeline_id }}
              </span>
              <span class="mt-0.5 flex items-center gap-1.5">
                <span
                  class="rounded-sm border border-border bg-muted/60 px-1 py-px text-[10px] font-medium uppercase tracking-wide text-muted-foreground"
                >
                  {{ triggerLabel(j.trigger_by) }}
                </span>
                <span class="text-[11px] text-muted-foreground/70">{{ timeAgo(j.created_at) }}</span>
              </span>
            </span>
            <span
              class="shrink-0 text-[10px] font-semibold uppercase tracking-wide"
              :class="
                j.status === 'success'
                  ? 'text-emerald-600'
                  : j.status === 'failed'
                    ? 'text-red-500'
                    : j.status === 'running'
                      ? 'text-amber-600'
                      : 'text-muted-foreground'
              "
            >
              {{ j.status }}
            </span>
          </a>
        </li>
      </ul>
    </div>
  </div>
</template>
