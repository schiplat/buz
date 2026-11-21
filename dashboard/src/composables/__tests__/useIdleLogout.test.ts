import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, defineComponent, h, nextTick, type App } from 'vue'

const { replace, logout, sessionPolicyApi } = vi.hoisted(() => ({
  replace: vi.fn(),
  logout: vi.fn(),
  sessionPolicyApi: { get: vi.fn(), update: vi.fn() },
}))

vi.mock('vue-router', () => ({ useRouter: () => ({ replace }) }))
vi.mock('@/stores/auth', () => ({
  useAuthStore: () => ({ isAuthenticated: true, logout }),
}))
vi.mock('@/api/sessionPolicy', () => ({ sessionPolicyApi }))

import { useIdleLogout } from '@/composables/useIdleLogout'

const Host = defineComponent({
  setup() {
    useIdleLogout()
    return () => h('div')
  },
})

let app: App | undefined

async function mount() {
  const root = document.createElement('div')
  document.body.appendChild(root)
  app = createApp(Host)
  app.mount(root)
  await vi.advanceTimersByTimeAsync(0)
  await nextTick()
}

function policy(idleMinutes: number) {
  return {
    ttl_hours: 24,
    ttl_default_hours: 24,
    ttl_overridden: false,
    idle_minutes: idleMinutes,
    idle_default_minutes: 0,
    idle_overridden: idleMinutes > 0,
  }
}

beforeEach(() => {
  vi.useFakeTimers()
  vi.clearAllMocks()
  localStorage.clear()
  sessionPolicyApi.get.mockResolvedValue(policy(1))
})

afterEach(() => {
  app?.unmount()
  app = undefined
  document.body.innerHTML = ''
  vi.useRealTimers()
})

describe('useIdleLogout', () => {
  it('signs the browser out after the configured idle window', async () => {
    await mount()

    await vi.advanceTimersByTimeAsync(30_000)
    expect(logout).not.toHaveBeenCalled()

    await vi.advanceTimersByTimeAsync(60_000)
    expect(logout).toHaveBeenCalledTimes(1)
    expect(replace).toHaveBeenCalledWith({ name: 'login', query: { reason: 'idle' } })
    expect(localStorage.getItem('buz_last_activity')).toBeNull()
  })

  it('does nothing when the policy disables the idle timeout', async () => {
    sessionPolicyApi.get.mockResolvedValue(policy(0))
    await mount()

    await vi.advanceTimersByTimeAsync(10 * 60_000)
    expect(logout).not.toHaveBeenCalled()
  })

  it('activity in the tab postpones the sign-out', async () => {
    await mount()

    await vi.advanceTimersByTimeAsync(45_000)
    window.dispatchEvent(new Event('pointerdown'))
    await vi.advanceTimersByTimeAsync(30_000)

    expect(logout).not.toHaveBeenCalled()

    await vi.advanceTimersByTimeAsync(60_000)
    expect(logout).toHaveBeenCalledTimes(1)
  })

  it('fails open when the policy cannot be read', async () => {
    sessionPolicyApi.get.mockRejectedValue(new Error('offline'))
    await mount()

    await vi.advanceTimersByTimeAsync(10 * 60_000)
    expect(logout).not.toHaveBeenCalled()
  })

  it('fails open when the policy payload has no usable idle value', async () => {
    // A Manager that predates the field (or a proxy answering with something
    // else) yields `undefined`; `undefined * 60_000` is NaN and must not arm
    // the timer — that regression caused a ~30 s logout loop.
    sessionPolicyApi.get.mockResolvedValue({ ttl_hours: 24 } as never)
    await mount()

    await vi.advanceTimersByTimeAsync(10 * 60_000)
    expect(logout).not.toHaveBeenCalled()
  })
})
