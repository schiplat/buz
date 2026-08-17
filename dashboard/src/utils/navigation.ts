import {
  isNavigationFailure,
  NavigationFailureType,
  type RouteLocationRaw,
  type Router,
} from 'vue-router'

const DEBUG_KEY = 'buz_debug_nav'

/** Enable with: localStorage.setItem('buz_debug_nav', '1') then reload */
export function isNavDebugEnabled(): boolean {
  try {
    if (localStorage.getItem(DEBUG_KEY) === '1') return true
  } catch {
    /* ignore */
  }
  return import.meta.env.DEV
}

export function navLog(...args: unknown[]) {
  if (isNavDebugEnabled()) {
    console.debug('[buz-nav]', ...args)
  }
}

/**
 * SPA navigate with a hard-navigation fallback.
 *
 * When the manager is down, in-flight API requests can exhaust the browser's
 * per-host connection pool. Lazy route imports then hang forever, so sidebar
 * RouterLink clicks appear dead while the href still works in a new tab.
 *
 * If `router.push` does not settle within `timeoutMs`, we fall back to a full
 * page load which uses a fresh connection context.
 */
export async function safeNavigate(
  router: Router,
  to: RouteLocationRaw,
  opts: { timeoutMs?: number; hardFallback?: boolean } = {},
): Promise<void> {
  const timeoutMs = opts.timeoutMs ?? 4_000
  const hardFallback = opts.hardFallback ?? true
  const resolved = router.resolve(to)
  const href = resolved.href

  navLog('start', { href, name: resolved.name, path: resolved.path })

  let timer: ReturnType<typeof setTimeout> | undefined
  const timeout = new Promise<'timeout'>((resolve) => {
    timer = setTimeout(() => resolve('timeout'), timeoutMs)
  })

  try {
    const raced = await Promise.race([
      router.push(to).then((failure) => ({ kind: 'done' as const, failure })),
      timeout,
    ])

    if (raced === 'timeout') {
      console.warn('[buz-nav] navigation timed out — hard fallback to', href)
      if (hardFallback) window.location.assign(href)
      return
    }

    if (raced.failure && isNavigationFailure(raced.failure)) {
      if (isNavigationFailure(raced.failure, NavigationFailureType.duplicated)) {
        navLog('duplicated (ok)', href)
        return
      }
      if (isNavigationFailure(raced.failure, NavigationFailureType.cancelled)) {
        navLog('cancelled', href)
        return
      }
      console.warn('[buz-nav] navigation failure', raced.failure)
      if (hardFallback) window.location.assign(href)
      return
    }

    navLog('ok', href)
  } catch (err) {
    console.error('[buz-nav] navigation threw', err)
    if (hardFallback) window.location.assign(href)
  } finally {
    if (timer) clearTimeout(timer)
  }
}
