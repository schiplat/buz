<script setup lang="ts">
import { formatLocalDateTime } from '@/utils/time'
import type { GitHubBind } from '@/api/githubSync'

defineProps<{
  binds: GitHubBind[]
  /** Show only the count + first chip? Set to true for a compact row summary. */
  compact?: boolean
}>()

defineEmits<{ open: [] }>()

function chipClass(status: string) {
  switch (status) {
    case 'ok':
      return 'bg-success/10 text-success dark:text-success '
    case 'failed':
      return 'bg-destructive/10 text-destructive dark:text-destructive '
    case 'stale':
      return 'bg-warning/10 text-warning dark:text-warning '
    default:
      return 'bg-warning/10 text-warning dark:text-warning '
  }
}

function dotClass(status: string) {
  switch (status) {
    case 'ok':
      return 'bg-success'
    case 'failed':
      return 'bg-destructive'
    case 'stale':
      return 'bg-warning'
    default:
      return 'bg-warning'
  }
}

function chipTitle(b: GitHubBind): string {
  switch (b.last_status) {
    case 'ok':
      return `Pushed to ${b.owner}/${b.repo}${b.last_pushed_at ? ` · ${formatLocalDateTime(b.last_pushed_at)}` : ''}`
    case 'failed':
      return b.last_op === 'unbind'
        ? `Unbind from ${b.owner}/${b.repo} failed: ${b.last_error || 'unknown error'}`
        : `Push to ${b.owner}/${b.repo} failed: ${b.last_error || 'unknown error'}`
    case 'stale':
      return `${b.owner}/${b.repo} — needs push (local value changed since last push)`
    default:
      return `${b.owner}/${b.repo} — bound, not pushed yet`
  }
}
</script>

<template>
  <div class="flex flex-wrap items-center gap-1" @click.stop>
    <button
      v-for="b in binds"
      :key="b.id"
      type="button"
      :title="chipTitle(b)"
      class="inline-flex cursor-pointer items-center gap-1 rounded-lg px-1.5 py-0.5 text-[10px] font-medium transition-colors hover:brightness-95"
      :class="chipClass(b.last_status)"
      @click="$emit('open')"
    >
      <span class="h-1.5 w-1.5 rounded-full" :class="dotClass(b.last_status)" />
      <span class="font-mono">{{ b.owner }}/{{ b.repo }}</span>
    </button>
  </div>
</template>
