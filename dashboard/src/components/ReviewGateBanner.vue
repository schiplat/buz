<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { RouterLink } from 'vue-router'
import { getStepReviewGate } from '@/api/health'

const gate = ref<{ manifest_configured: boolean; require_reviewed: boolean } | null>(null)
const dismissed = ref(false)

onMounted(async () => {
  // Null = probe failed or manager predates the field: stay silent rather
  // than nag on every page when we cannot actually know the posture.
  gate.value = (await getStepReviewGate()) ?? null
})

/** Posture drives both visibility and tone:
 *  - strict + no manifest  → ERROR: every template is rejected right now;
 *  - no gate at all        → WARNING: templates resolve unchecked;
 *  - manifest configured   → the gate is working, banner stays silent. */
const posture = computed<'strict-without-manifest' | 'no-gate' | null>(() => {
  const g = gate.value
  if (!g) return null
  if (g.manifest_configured) return null
  return g.require_reviewed ? 'strict-without-manifest' : 'no-gate'
})

const isError = computed(() => posture.value === 'strict-without-manifest')
const show = computed(() => posture.value !== null && !dismissed.value)
</script>

<template>
  <div
    v-if="show"
    role="status"
    data-testid="review-gate-banner"
    :class="[
      'mb-4 flex items-start gap-3 rounded-xl border px-4 py-3 text-sm',
      isError ? 'border-red-500/40 bg-red-500/5' : 'border-warning/30 bg-warning/5',
    ]"
  >
    <span aria-hidden="true" class="text-base leading-5">🛡️</span>
    <div class="min-w-0 flex-1">
      <template v-if="isError">
        <p class="font-medium text-red-600 dark:text-red-400">
          Step review gate is misconfigured — every template will be rejected
        </p>
        <p class="mt-0.5 wrap-break-word text-muted-foreground">
          The switch is on, but its review ledger (reviews.json) cannot be found — until the file
          exists, no plugin can pass. Fix: clone
          <span class="font-mono text-[13px]">buz-plugins</span> into your data directory, or set the
          manifest path under
          <RouterLink to="/plugin-library/sources" class="font-medium underline underline-offset-2">Plugins → Sources &amp; Settings</RouterLink>.
        </p>
      </template>
      <template v-else>
        <p class="font-medium text-warning">Step review gate is not enabled</p>
        <p class="mt-0.5 wrap-break-word text-muted-foreground">
          Templates are used without review checks. To turn checking on, first place the review
          ledger (a buz-plugins clone provides <span class="font-mono text-[13px]">reviews.json</span>),
          then flip the switch in
          <RouterLink to="/plugin-library/sources" class="font-medium underline underline-offset-2">Plugins → Sources &amp; Settings</RouterLink>.
        </p>
      </template>
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
