import { onMounted, onUnmounted } from 'vue'
import { useRouter } from 'vue-router'
import { useAuthStore } from '@/stores/auth'
import { sessionPolicyApi } from '@/api/sessionPolicy'

/**
 * Client-side idle sign-out (docs `platform/oidc-sso.md`).
 *
 * This is a **convenience, not a security control**: it clears the browser's
 * token after inactivity, but an already-issued JWT stays valid until its
 * `exp` (or an IdP-initiated account-wide revocation). The server-side
 * boundary is the session lifetime, and the copy in the settings UI says so.
 *
 * Activity is shared across tabs through `localStorage`, so working in one
 * tab keeps every other tab alive; the timestamp write is throttled because
 * pointer events fire continuously.
 */

const ACTIVITY_KEY = 'buz_last_activity'
const CHECK_INTERVAL_MS = 30_000
const ACTIVITY_WRITE_THROTTLE_MS = 5_000
const ACTIVITY_EVENTS = ['pointerdown', 'keydown', 'wheel', 'touchstart'] as const

function readLastActivity(): number {
  try {
    const raw = localStorage.getItem(ACTIVITY_KEY)
    const parsed = raw ? Number(raw) : NaN
    return Number.isFinite(parsed) ? parsed : Date.now()
  } catch {
    // Storage can throw in private mode — treat "unknown" as "just active".
    return Date.now()
  }
}

export function useIdleLogout() {
  const router = useRouter()
  const auth = useAuthStore()

  let idleMs = 0
  let lastActivityWrite = 0
  let checkTimer: number | undefined

  function markActivity() {
    const now = Date.now()
    if (now - lastActivityWrite < ACTIVITY_WRITE_THROTTLE_MS) return
    lastActivityWrite = now
    try {
      localStorage.setItem(ACTIVITY_KEY, String(now))
    } catch {
      // no durable storage — the in-memory default keeps the tab usable
    }
  }

  function check() {
    if (idleMs <= 0 || !auth.isAuthenticated) return
    if (Date.now() - readLastActivity() < idleMs) return
    idleMs = 0 // disarm: one redirect, no repeated timers
    auth.logout()
    try {
      localStorage.removeItem(ACTIVITY_KEY)
    } catch {
      // nothing to clean up
    }
    void router.replace({ name: 'login', query: { reason: 'idle' } })
  }

  function teardown() {
    window.clearInterval(checkTimer)
    checkTimer = undefined
    for (const type of ACTIVITY_EVENTS) window.removeEventListener(type, markActivity)
  }

  onMounted(async () => {
    try {
      const policy = await sessionPolicyApi.get()
      // A malformed payload must NOT arm the timer: `undefined * 60_000` is
      // NaN, and `NaN <= 0` is false — the guard below would then let the
      // first tick sign the browser out (a ~30 s logout loop). This happens
      // when a freshly built SPA runs against an older Manager (or a proxy)
      // that answers without `idle_minutes`.
      const minutes = Number(policy?.idle_minutes)
      idleMs = Number.isFinite(minutes) && minutes > 0 ? minutes * 60_000 : 0
    } catch {
      return // policy unavailable: no idle enforcement (fail open, it is UX only)
    }
    if (idleMs <= 0) return

    markActivity()
    for (const type of ACTIVITY_EVENTS) {
      window.addEventListener(type, markActivity, { passive: true })
    }
    checkTimer = window.setInterval(check, CHECK_INTERVAL_MS)
  })

  onUnmounted(teardown)
}
