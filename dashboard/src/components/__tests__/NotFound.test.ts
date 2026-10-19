import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createWebHistory } from 'vue-router'
import { createApp, type App as VueApp } from 'vue'
import NotFound from '@/pages/NotFound.vue'
import ServerErrorBanner from '@/components/ServerErrorBanner.vue'
import { useApiFailuresStore } from '@/stores/apiFailures'

function mountComponent(component: VueApp['component'], plugins: unknown[]) {
  const el = document.createElement('div')
  document.body.appendChild(el)
  const app = createApp(component)
  for (const plugin of plugins) {
    app.use(plugin as never)
  }
  app.mount(el)
  return {
    el,
    text: () => el.textContent ?? '',
    clickButton: async (label: string) => {
      const button = Array.from(el.querySelectorAll('button')).find(
        (b) => b.textContent?.trim() === label,
      )
      if (!button) throw new Error(`button not found: ${label}`)
      button.click()
      await Promise.resolve()
    },
    queryAlert: () => el.querySelector('[role="alert"]'),
    clickDismiss: async () => {
      const button = el.querySelector<HTMLButtonElement>('button[aria-label="Dismiss error notice"]')
      if (!button) throw new Error('dismiss button not found')
      button.click()
      await Promise.resolve()
    },
    unmount: () => {
      app.unmount()
      el.remove()
    },
  }
}

describe('NotFound page', () => {
  let mounted: ReturnType<typeof mountComponent> | undefined

  afterEach(() => {
    mounted?.unmount()
    mounted = undefined
  })

  async function mountAtUnknownPath() {
    const router = createRouter({
      history: createWebHistory(),
      routes: [
        { path: '/dashboard', name: 'dashboard', component: { template: '<div />' } },
        { path: '/:pathMatch(.*)*', name: 'not-found', component: NotFound },
      ],
    })
    await router.push('/nope')
    await router.isReady()
    mounted = mountComponent(NotFound, [router])
    return { router }
  }

  it('renders a friendly 404 with copy and actions', async () => {
    await mountAtUnknownPath()
    expect(mounted!.text()).toContain('404')
    expect(mounted!.text()).toContain('Page not found')
    expect(mounted!.text()).toContain('Go back')
    expect(mounted!.text()).toContain('Go to Overview')
  })

  it('routes to Overview from the primary action', async () => {
    const { router } = await mountAtUnknownPath()
    await mounted!.clickButton('Go to Overview')
    // Router navigation settles over several microtasks + a tick.
    for (let i = 0; i < 5; i += 1) {
      await Promise.resolve()
      await new Promise((resolve) => setTimeout(resolve, 0))
    }
    expect(router.currentRoute.value.name).toBe('dashboard')
  })
})

describe('ServerErrorBanner', () => {
  let pinia: ReturnType<typeof createPinia>
  let mounted: ReturnType<typeof mountComponent> | undefined

  beforeEach(() => {
    pinia = createPinia()
    setActivePinia(pinia)
    useApiFailuresStore().resetForTests()
  })

  afterEach(() => {
    mounted?.unmount()
    mounted = undefined
  })

  function mountBanner() {
    mounted = mountComponent(ServerErrorBanner, [pinia])
    return mounted
  }

  it('hides when no failure is reported', () => {
    mountBanner()
    expect(mounted!.queryAlert()).toBeNull()
  })

  it('shows a friendly message for 5xx and dismisses', async () => {
    const view = mountBanner()
    useApiFailuresStore().report(502, 'bad gateway')
    await Promise.resolve()
    const alert = view.queryAlert()
    expect(alert).not.toBeNull()
    expect(alert!.textContent).toContain('Server error')
    expect(alert!.textContent).toContain('HTTP 502')
    await view.clickDismiss()
    expect(view.queryAlert()).toBeNull()
  })

  it('uses an unreachable-manager message for network failures', async () => {
    const view = mountBanner()
    useApiFailuresStore().report(0, 'Network Error')
    await Promise.resolve()
    expect(view.queryAlert()!.textContent).toContain('unreachable')
  })
})
