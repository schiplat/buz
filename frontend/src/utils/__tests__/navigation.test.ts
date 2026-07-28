/**
 * Unit tests for safeNavigate — run with: cd frontend && npx vitest run
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { createRouter, createMemoryHistory } from 'vue-router'
import { safeNavigate } from '../navigation'

describe('safeNavigate', () => {
  const assign = vi.fn()

  beforeEach(() => {
    assign.mockReset()
    vi.stubGlobal('location', { ...window.location, assign })
  })

  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('resolves when router.push succeeds quickly', async () => {
    const router = createRouter({
      history: createMemoryHistory(),
      routes: [
        { path: '/', name: 'home', component: { template: '<div/>' } },
        { path: '/dashboard', name: 'dashboard', component: { template: '<div/>' } },
      ],
    })
    await router.push('/')
    await safeNavigate(router, '/dashboard', { timeoutMs: 2_000, hardFallback: true })
    expect(router.currentRoute.value.path).toBe('/dashboard')
    expect(assign).not.toHaveBeenCalled()
  })

  it('hard-falls back when navigation hangs past timeout', async () => {
    const router = createRouter({
      history: createMemoryHistory(),
      routes: [
        { path: '/', name: 'home', component: { template: '<div/>' } },
        {
          path: '/slow',
          name: 'slow',
          component: () => new Promise(() => { /* never resolves — simulates hung chunk load */ }),
        },
      ],
    })
    await router.push('/')
    await safeNavigate(router, '/slow', { timeoutMs: 50, hardFallback: true })
    expect(assign).toHaveBeenCalled()
    const href = String(assign.mock.calls[0][0])
    expect(href).toContain('slow')
  })
})
