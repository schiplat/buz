import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import WaitingForAccess from '@/pages/WaitingForAccess.vue'
import type { TeamMembership } from '@/api/auth'

const { auth, route, router } = vi.hoisted(() => ({
  auth: {
    username: 'carol',
    teams: [] as TeamMembership[],
    isPlatformAdmin: false,
    isSuperAdmin: false,
    isOps: false,
    fetchMe: vi.fn(async () => {}),
    logout: vi.fn(),
  },
  route: { query: {} as Record<string, unknown> },
  router: { replace: vi.fn(async () => {}) },
}))

vi.mock('vue-router', () => ({ useRoute: () => route, useRouter: () => router }))
vi.mock('@/stores/auth', () => ({ useAuthStore: () => auth }))

let app: App | undefined

const flush = async () => {
  for (let i = 0; i < 3; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0))
    await nextTick()
  }
}

function mount() {
  const host = document.createElement('div')
  document.body.appendChild(host)
  app = createApp(WaitingForAccess)
  app.mount(host)
  return host
}

function buttonByText(text: string): HTMLButtonElement {
  const element = [...document.querySelectorAll<HTMLButtonElement>('button')].find((node) =>
    node.textContent?.includes(text),
  )
  expect(element, `Missing button containing "${text}"`).toBeTruthy()
  return element!
}

beforeEach(() => {
  auth.username = 'carol'
  auth.teams = []
  auth.isPlatformAdmin = false
  auth.isSuperAdmin = false
  auth.isOps = false
  route.query = {}
  vi.mocked(auth.fetchMe).mockClear()
  vi.mocked(router.replace).mockClear()
})

afterEach(() => {
  app?.unmount()
  app = undefined
  document.body.innerHTML = ''
})

describe('Waiting for access', () => {
  it('keeps a zero-team account out of the shell and re-checks on demand', async () => {
    mount()
    await flush()

    expect(document.body.textContent).toContain('Waiting for access')
    expect(document.body.textContent).toContain('carol')
    expect(document.body.textContent).toContain("isn't in a team yet")

    buttonByText('Check again').click()
    await flush()

    expect(vi.mocked(auth.fetchMe)).toHaveBeenCalled()
    expect(document.body.textContent).toContain('Still no team yet')
    expect(vi.mocked(router.replace)).not.toHaveBeenCalled()
  })

  it('enters the app as soon as a team (or admin role) appears', async () => {
    route.query = { redirect: '/pipelines' }
    mount()
    await flush()

    auth.teams = [{
      id: 't-alpha',
      slug: 'team-alpha',
      name: 'team-alpha',
      role: 'developer',
    } as TeamMembership]
    buttonByText('Check again').click()
    await flush()

    expect(vi.mocked(router.replace)).toHaveBeenCalledWith('/pipelines')
  })

  it('sends platform admins through without requiring a team', async () => {
    auth.isPlatformAdmin = true
    mount()
    await flush()

    buttonByText('Check again').click()
    await flush()

    expect(vi.mocked(router.replace)).toHaveBeenCalledWith('/dashboard')
  })
})
