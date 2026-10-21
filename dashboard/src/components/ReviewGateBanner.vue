<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { getStepReviewGate } from '@/api/health'

const show = ref(false)
const dismissed = ref(false)

onMounted(async () => {
  const gate = await getStepReviewGate()
  // Null = probe failed or manager predates the field: stay silent rather
  // than nag on every page when we cannot actually know the posture.
  if (gate && !gate.manifest_configured) show.value = true
})
</script>

<template>
  <div
    v-if="show && !dismissed"
    role="status"
    data-testid="review-gate-banner"
    class="mb-4 flex items-start gap-3 rounded-xl border border-warning/30 bg-warning/5 px-4 py-3 text-sm"
  >
    <span aria-hidden="true" class="text-base leading-5">🛡️</span>
    <div class="min-w-0 flex-1">
      <p class="font-medium text-warning">Step review gate is not enabled</p>
      <p class="mt-0.5 wrap-break-word text-muted-foreground">
        Every <code>uses:</code> template resolves without review checks. For production, set
        <code>[step_index] reviewed_manifest</code> (and
        <code>require_reviewed = true</code>) in manager.toml — see the Configuration docs.
      </p>
    </div>
    <button
      type="button"
      class="shrink-0 rounded-lg border border-border px-2 py-1 text-xs hover:bg-muted"
      aria-label="Dismiss review gate notice"
      @click="dismissed = true"
    >
      Dismiss
    </button>
  </div>
</template>
