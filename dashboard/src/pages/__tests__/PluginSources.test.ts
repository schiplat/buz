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
    getSyncProgress: vi.fn(),
  },
}))
vi.mock('@/api/adminSettings', () => ({
  adminSettingsApi: { list: vi.fn(), update: vi.fn(), reset: vi.fn() },
}))
vi.mock('@/composables/useAppDialog', () => ({ useAppDialog: () => dialog }))
vi.mock('vue-router', () => ({
  useRouter: () => ({ replace: vi.fn(), push: vi.fn() }),
  useRoute: () => ({ query: {} as Record<string, string> }),
}))

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
        value: '/srv/plugins',
        default: '/srv/plugins',
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

  it('shows "syncing…" for a source whose last_synced_at is unchanged after a click', async () => {
    const root = await mountPage()
    const sync = [...root.querySelectorAll<HTMLButtonElement>('button')]
      .find((b) => b.textContent?.includes('Sync'))!
    sync.click()
    await flush()
    // The refreshed list carries the SAME last_synced_at → still in flight.
    expect(root.textContent).toContain('syncing…')
    expect(root.textContent).not.toContain('synced 2026-10-23T00:00:00Z')
  })

  it('clears "syncing…" once last_synced_at changes (success or failure)', async () => {
    vi.mocked(stepSourcesApi.listSources)
      .mockResolvedValueOnce({ items: [source()] })
      .mockResolvedValue({
        items: [source({ last_synced_at: '2026-10-25T01:00:00Z' })],
      })
    const root = await mountPage()
    const sync = [...root.querySelectorAll<HTMLButtonElement>('button')]
      .find((b) => b.textContent?.includes('Sync'))!
    sync.click()
    await flush()
    expect(root.textContent).toContain('synced 2026-10-25T01:00:00Z')
    expect(root.textContent).not.toContain('syncing…')
  })

  it('shows a live progress bar with true counts while a sync runs', async () => {
    vi.mocked(stepSourcesApi.getSyncProgress).mockResolvedValue({
      phase: 'processing',
      total_plugins: 29,
      done_plugins: 3,
      total_entries: 45,
      done_entries: 12,
      error: null,
    })
    const root = await mountPage()
    const sync = [...root.querySelectorAll<HTMLButtonElement>('button')]
      .find((b) => b.textContent?.includes('Sync'))!
    sync.click()
    await flush()
    // The first progress tick runs immediately after the click.
    expect(root.textContent).toContain('Syncing 12/45 entries · 3/29 plugins')
    const bar = root.querySelector<HTMLElement>('.rounded-full.bg-primary')
    expect(bar?.style.width).toBe('27%')
  })

  it('surfaces a failed sync in red through the progress bar', async () => {
    vi.mocked(stepSourcesApi.getSyncProgress).mockResolvedValue({
      phase: 'failed',
      total_plugins: 29,
      done_plugins: 0,
      total_entries: 45,
      done_entries: 0,
      error: 'source unreachable (connection failed)',
    })
    const root = await mountPage()
    const sync = [...root.querySelectorAll<HTMLButtonElement>('button')]
      .find((b) => b.textContent?.includes('Sync'))!
    sync.click()
    await flush()
    expect(root.textContent).toContain('Sync failed: source unreachable (connection failed)')
  })

  it('adds a source, returns to the list, and shows the syncing state', async () => {
    vi.mocked(stepSourcesApi.createSource).mockResolvedValue(undefined)
    // After the add, the refreshed list carries the new source in its
    // in-flight state (no last_synced_at, no error yet).
    vi.mocked(stepSourcesApi.listSources)
      .mockResolvedValueOnce({ items: [source()] })
      .mockResolvedValue({
        items: [
          source(),
          source({ id: 'src-2', name: 'bob', url: 'https://example.com/bob/index.json', last_synced_at: null }),
        ],
      })
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
    // The form closed (back to the list) and the new row shows the
    // in-flight state while the background sync runs.
    expect(root.textContent).not.toContain('Index URL')
    expect(root.textContent).toContain('syncing…')
  })

  it('renders the four platform settings with effective values', async () => {
    const root = await mountPage()
    const keys = [...root.querySelectorAll('h3')].map((h) => h.textContent)
    expect(keys).not.toContain('Marketplace URL')
    expect(keys).toContain('Step review gate')
    expect(keys).toContain('Local plugin directory')
    expect(keys).toContain('Reviewed manifest')
    // Input values are not part of textContent — assert the element itself.
    const dirInput = [...root.querySelectorAll('input')].find((i) => i.value === '/srv/plugins')!
    expect(dirInput, 'local dir input carries the effective value').toBeTruthy()
  })

  it('applies the review-gate toggle immediately', async () => {
    const root = await mountPage()
    const toggle = root.querySelector<HTMLElement>('[data-testid="switch-step_review_required"]')!
    expect(toggle.getAttribute('aria-checked')).toBe('false')
    toggle.click()
    await flush()
    expect(adminSettingsApi.update).toHaveBeenCalledWith('step_review_required', 'true')
  })

  it('hints inline when the configured manifest file does not exist', async () => {
    vi.mocked(adminSettingsApi.list).mockResolvedValue({
      items: [
        platformSetting({ key: 'step_reviewed_manifest', kind: 'path', value: '/no/such/reviews.json', fileState: 'missing' }),
      ],
    })
    const root = await mountPage()
    expect(root.querySelector('[data-testid="manifest-missing-hint"]')).not.toBeNull()
    expect(root.textContent).toContain('File not found')
  })

  it('warns loudly when the sync timeout is 0 (disabled)', async () => {
    vi.mocked(adminSettingsApi.list).mockResolvedValue({
      items: [
        platformSetting({ key: 'step_sync_timeout', kind: 'number', value: '0' }),
      ],
    })
    const root = await mountPage()
    expect(root.querySelector('[data-testid="timeout-disabled-warning"]')).not.toBeNull()
    expect(root.textContent).toContain('Timeout disabled')
  })

  it('saves an edited path setting and resets to the manager.toml default', async () => {
    const root = await mountPage()
    const dir = root.querySelector<HTMLInputElement>('[data-testid="input-step_index_local_dir"]')!
    dir.value = '/srv/plugins-v2'
    dir.dispatchEvent(new Event('input', { bubbles: true }))
    await nextTick()
    const save = [...root.querySelectorAll('button')].find((b) => b.textContent?.trim() === 'Save')!
    save.click()
    await flush()
    expect(adminSettingsApi.update).toHaveBeenCalledWith('step_index_local_dir', '/srv/plugins-v2')

    const reset = root.querySelector<HTMLElement>('[data-testid="reset-step_index_local_dir"]')!
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
