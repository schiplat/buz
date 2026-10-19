<script setup lang="ts">
import { useApiFailuresStore } from '@/stores/apiFailures'

const failures = useApiFailuresStore()

function describe(f: { status: number; message: string }): string {
  if (f.status === 0) {
    return 'Cannot reach the Buz Manager — check the connection and try again shortly.'
  }
  return `The server answered with HTTP ${f.status}. Your data is safe — retry in a moment, or contact the administrator if it persists.`
}
</script>

<template>
  <div
    v-if="failures.current"
    role="alert"
    class="mb-4 flex items-start gap-3 rounded-xl border border-destructive/30 bg-destructive/5 px-4 py-3 text-sm"
  >
    <span aria-hidden="true" class="text-base leading-5">⚠️</span>
    <div class="min-w-0 flex-1">
      <p class="font-medium text-destructive">
        {{
          failures.current.status === 0
            ? 'Buz Manager is unreachable'
            : 'Server error'
        }}
      </p>
      <p class="mt-0.5 break-words text-muted-foreground">
        {{ describe(failures.current) }}
      </p>
    </div>
    <button
      type="button"
      class="shrink-0 rounded-lg border border-border px-2 py-1 text-xs hover:bg-muted"
      aria-label="Dismiss error notice"
      @click="failures.dismiss()"
    >
      Dismiss
    </button>
  </div>
</template>
