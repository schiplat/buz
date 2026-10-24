import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import PluginSources from '@/pages/PluginSources.vue'
import { stepSourcesApi, type StepSource } from '@/api/stepSources'

const { dialog } = vi.hoisted(() => ({
  dialog: { alert: vi.fn(), confirm: vi.fn() },
}))

vi.mock('@/api/stepSources', () => ({
  stepSourcesApi: {
    listSources: vi.fn(),
    createSource: vi.fn(),
    syncSource: vi.fn(),
    deleteSource: vi.fn(),
  },
}))
vi.mock('@/composables/useAppDialog', () => ({ useAppDialog: () => dialog }))

const source = (overrides: Partial<StepSource> = {}): StepSource => ({
  id: 'src-1',
  name: 'alice',
  url: 'https://example.com/alice/index.json',
  auto_enable: false,
  enabled: true,
  last_synced_at: '2026-10-23T00:00:00Z',
  last_sync_error: null,
  ...overrides,
})

let app: App | undefined
const flush = async () => {
  await new Promise((resolve) => setTimeout(resolve, 0))
  await nextTick()
}

async function mountPage() {
  const root = document.createElement('div')
  document.body.appendChild(root)
  app = createApp(PluginSources)
  app.mount(root)
  await flush()
  return root
}

beforeEach(() => {
  vi.resetAllMocks()
  dialog.alert.mockResolvedValue(undefined)
  dialog.confirm.mockResolvedValue(true)
  vi.mocked(stepSourcesApi.listSources).mockResolvedValue({ items: [source()] })
})

afterEach(() => {
  app?.unmount()
  app = undefined
  document.body.innerHTML = ''
})

describe('PluginSources', () => {
  it('renders configured sources with sync state', async () => {
    const root = await mountPage()
    expect(root.textContent).toContain('alice')
    expect(root.textContent).toContain('synced 2026-10-23T00:00:00Z')
  })

  it('adds a source and syncs immediately', async () => {
    vi.mocked(stepSourcesApi.createSource).mockResolvedValue({ synced: true })
    const root = await mountPage()
    const opener = [...root.querySelectorAll('button')].find((b) => b.textContent?.includes('Add source'))!
    opener.click()
    await nextTick()
    root.querySelector<HTMLInputElement>('input[type="text"]')!.value = 'bob'
    root.querySelector('input[type="text"]')!.dispatchEvent(new Event('input', { bubbles: true }))
    root.querySelector<HTMLInputElement>('input[type="url"]')!.value = 'https://example.com/bob/index.json'
    root.querySelector('input[type="url"]')!.dispatchEvent(new Event('input', { bubbles: true }))
    await nextTick()

    const add = [...root.querySelectorAll<HTMLButtonElement>('button')]
      .find((b) => b.textContent?.includes('Add & sync'))!
    add.click()
    await flush()
    expect(stepSourcesApi.createSource).toHaveBeenCalledWith('bob', 'https://example.com/bob/index.json', false)
  })

  it('surfaces a failed initial sync without throwing', async () => {
    const root = await mountPage()
    const opener = [...root.querySelectorAll('button')].find((b) => b.textContent?.includes('Add source'))!
    opener.click()
    await nextTick()
    vi.mocked(stepSourcesApi.createSource).mockRejectedValue(new Error('boom'))
    root.querySelector<HTMLInputElement>('input[type="text"]')!.value = 'bob'
    root.querySelector('input[type="text"]')!.dispatchEvent(new Event('input', { bubbles: true }))
    root.querySelector<HTMLInputElement>('input[type="url"]')!.value = 'https://example.com/bob/index.json'
    root.querySelector('input[type="url"]')!.dispatchEvent(new Event('input', { bubbles: true }))
    await nextTick()
    const add = [...root.querySelectorAll<HTMLButtonElement>('button')]
      .find((b) => b.textContent?.includes('Add & sync'))!
    add.click()
    await flush()
    expect(dialog.alert).toHaveBeenCalled()
  })
})
