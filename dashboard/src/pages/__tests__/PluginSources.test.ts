import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import PluginSources from '@/pages/PluginSources.vue'
import { stepSourcesApi, type StepSource } from '@/api/stepSources'
import { adminSettingsApi, type PlatformSetting } from '@/api/adminSettings'

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
vi.mock('@/api/adminSettings', () => ({
  adminSettingsApi: { list: vi.fn(), update: vi.fn(), reset: vi.fn() },
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

const platformSetting = (overrides: Partial<PlatformSetting>): PlatformSetting => ({
  key: 'step_review_required',
  kind: 'bool',
  value: 'false',
  default: 'false',
  overridden: false,
  ...overrides,
})

beforeEach(() => {
  vi.resetAllMocks()
  dialog.alert.mockResolvedValue(undefined)
  dialog.confirm.mockResolvedValue(true)
  vi.mocked(stepSourcesApi.listSources).mockResolvedValue({ items: [source()] })
  vi.mocked(adminSettingsApi.list).mockResolvedValue({
    items: [
      platformSetting({ key: 'marketplace_url', kind: 'url', value: '' }),
      platformSetting({ key: 'step_review_required', value: 'false' }),
      platformSetting({
        key: 'step_index_local_dir',
        kind: 'path',
        value: '/srv/steps',
        default: '/srv/steps',
      }),
      platformSetting({ key: 'step_reviewed_manifest', kind: 'path', value: '' }),
      platformSetting({ key: 'step_sync_timeout', kind: 'number', value: '60', default: '60' }),
    ],
  })
  vi.mocked(adminSettingsApi.update).mockImplementation(async (key, value) => ({ key, value }))
  vi.mocked(adminSettingsApi.reset).mockImplementation(async (key) => ({ key }))
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
    expect(stepSourcesApi.createSource).toHaveBeenCalledWith('bob', 'https://example.com/bob/index.json', false, 60000)
  })

  it('renders the four platform settings with effective values', async () => {
    const root = await mountPage()
    const keys = [...root.querySelectorAll('h3')].map((h) => h.textContent)
    expect(keys).toContain('Marketplace URL')
    expect(keys).toContain('Step review gate (require_reviewed)')
    expect(keys).toContain('Local step directory')
    expect(keys).toContain('Reviewed manifest')
    // Input values are not part of textContent — assert the element itself.
    const dirInput = [...root.querySelectorAll('input')].find((i) => i.value === '/srv/steps')!
    expect(dirInput, 'local dir input carries the effective value').toBeTruthy()
  })

  it('applies the review-gate toggle immediately', async () => {
    const root = await mountPage()
    const toggle = root.querySelector<HTMLInputElement>('input[type="checkbox"]')!
    expect(toggle.checked).toBe(false)
    toggle.checked = true
    toggle.dispatchEvent(new Event('change', { bubbles: true }))
    await flush()
    expect(adminSettingsApi.update).toHaveBeenCalledWith('step_review_required', 'true')
  })

  it('saves an edited path setting and resets to the manager.toml default', async () => {
    const root = await mountPage()
    const dir = [...root.querySelectorAll('input')].find((i) => i.value === '/srv/steps')!
    const card = dir.closest('.px-5')!
    dir.value = '/srv/steps-v2'
    dir.dispatchEvent(new Event('input', { bubbles: true }))
    await nextTick()
    const save = [...card.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Save')!
    save.click()
    await flush()
    expect(adminSettingsApi.update).toHaveBeenCalledWith('step_index_local_dir', '/srv/steps-v2')

    const reset = [...card.querySelectorAll('button')].find((b) => b.textContent?.includes('Reset'))!
    reset.click()
    await flush()
    expect(adminSettingsApi.reset).toHaveBeenCalledWith('step_index_local_dir')
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
