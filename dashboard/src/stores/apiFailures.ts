import { defineStore } from 'pinia'
import { ref } from 'vue'

/** One friendly global notice about server/network trouble (5xx or unreachable). */
export interface ApiFailure {
  /** HTTP status (500, 502, …) or 0 for network-level failures. */
  status: number
  message: string
  at: number
}

const DEDUPE_WINDOW_MS = 5_000

export const useApiFailuresStore = defineStore('apiFailures', () => {
  /** Newest failure; replaced by later ones, cleared on dismiss. */
  const current = ref<ApiFailure | null>(null)
  let lastReportedAt = 0

  /**
   * Report a failure. Bursts of failing requests (a dashboard fires several
   * calls at once) collapse into one notice; a different status or a later
   * window replaces the shown one.
   */
  function report(status: number, message: string) {
    const now = Date.now()
    if (
      current.value &&
      current.value.status === status &&
      now - lastReportedAt < DEDUPE_WINDOW_MS
    ) {
      return
    }
    lastReportedAt = now
    current.value = { status, message, at: now }
  }

  function dismiss() {
    current.value = null
  }

  function resetForTests() {
    current.value = null
    lastReportedAt = 0
  }

  return { current, report, dismiss, resetForTests }
})
