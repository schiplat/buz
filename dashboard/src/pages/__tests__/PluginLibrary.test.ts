import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import PluginLibrary from '@/pages/PluginLibrary.vue'

const push = vi.fn()

vi.mock('vue-router', () => ({
  RouterView: { template: '<div data-testid="router-view">child</div>' },
  useRoute: () => ({ name: 'plugin-pool' }),
  useRouter: () => ({ push }),
}))

let app: App | undefined

async function mountShell() {
  const root = document.createElement('div')
  document.body.appendChild(root)
  app = createApp(PluginLibrary)
  app.mount(root)
  await nextTick()
  return root
}

afterEach(() => {
  app?.unmount()
  app = undefined
  document.body.innerHTML = ''
})

describe('PluginLibrary shell', () => {
  it('renders the tab bar with both tabs and the child outlet', async () => {
    const root = await mountShell()
    const tabs = [...root.querySelectorAll('button')].map((b) => b.textContent?.trim())
    expect(tabs).toEqual(['Pool', 'Sources & Settings'])
    // Regression: a scripted cleanup once emptied this div entirely, leaving
    // no way to switch tabs.
    expect(root.querySelector('[data-testid="router-view"]')).not.toBeNull()
  })

  it('marks the active tab and navigates on click', async () => {
    const root = await mountShell()
    const [poolTab, sourcesTab] = [...root.querySelectorAll('button')]
    // Token match: 'hover:bg-background/60' contains 'bg-background' as a
    // substring and would false-positive a naive includes().
    const isActive = (el: HTMLButtonElement) =>
      el.className.split(/\s+/).includes('bg-background')
    expect(isActive(poolTab)).toBe(true)
    expect(isActive(sourcesTab)).toBe(false)

    sourcesTab.click()
    await nextTick()
    expect(push).toHaveBeenCalledWith({ name: 'plugin-sources' })
  })
})
