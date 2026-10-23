import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import PluginLibrary from '@/pages/PluginLibrary.vue'
import {
  stepSourcesApi,
  stepsApi,
  type PoolEntry,
  type StepSource,
} from '@/api/stepSources'

const { dialog, auth } = vi.hoisted(() => ({
  dialog: { alert: vi.fn(), confirm: vi.fn().mockResolvedValue(true) },
  auth: { isPlatformAdmin: true },
}))

vi.mock('@/stores/auth', () => ({ useAuthStore: () => auth }))
vi.mock('@/composables/useAppDialog', () => ({ useAppDialog: () => dialog }))
vi.mock('@/api/stepSources', () => ({
  stepSourcesApi: {
    listSources: vi.fn(),
    createSource: vi.fn(),
    syncSource: vi.fn(),
    deleteSource: vi.fn(),
    listPool: vi.fn(),
    togglePoolEntry: vi.fn(),
  },
  stepsApi: { listLocal: vi.fn() },
}))
vi.mock('vue-router', () => ({ useRouter: () => ({ replace: vi.fn() }) }))

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

const poolEntry = (overrides: Partial<PoolEntry>): PoolEntry => ({
  namespace: 'alice',
  step_id: 'oss-upload',
  version: '1.0.0',
  sha256: 'a'.repeat(64),
  source_id: 'src-1',
  source_name: 'alice',
  file_path: 'steps/oss-upload/1.0.0.yaml',
  enabled: false,
  reviewed: false,
  added_by: 'sync',
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
  app = createApp(PluginLibrary)
  app.mount(root)
  await flush()
  return root
}

beforeEach(() => {
  vi.resetAllMocks()
  dialog.alert.mockResolvedValue(undefined)
  dialog.confirm.mockResolvedValue(true)
  vi.mocked(stepSourcesApi.listSources).mockResolvedValue({ items: [source()] })
  vi.mocked(stepSourcesApi.listPool).mockResolvedValue({
    items: [
      poolEntry({ version: '1.0.0', enabled: true, reviewed: true }),
      poolEntry({ version: '1.1.0', enabled: false }),
    ],
  })
  vi.mocked(stepsApi.listLocal).mockResolvedValue({
    items: [
      { id: 'buz/checkout', version: '1.0.0' },
      { id: 'buz/checkout', version: '1.1.0' },
    ],
    unparseable: [],
  })
})

afterEach(() => {
  app?.unmount()
  app = undefined
  document.body.innerHTML = ''
})

describe('PluginLibrary', () => {
  it('lists every plugin with its full version list, newest first', async () => {
    const root = await mountPage()

    const groupHeader = [...root.querySelectorAll('code')].find((el) =>
      el.textContent?.includes('alice/oss-upload'),
    )
    expect(groupHeader, 'pool group header').toBeTruthy()

    const versions = [...root.querySelectorAll('code')]
      .filter((el) => el.textContent?.startsWith('v'))
      .map((el) => el.textContent)
    expect(versions).toEqual(['v1.1.0', 'v1.0.0'])
  })

  it('shows state badges and the update-available verdict per group', async () => {
    const root = await mountPage()
    const badges = [...root.querySelectorAll('span')].map((s) => s.textContent)
    expect(badges).toContain('enabled')
    expect(badges).toContain('staged')
    expect(badges).toContain('reviewed')
    expect(badges).toContain('update available')
  })

  it('groups local templates by id with all versions', async () => {
    const root = await mountPage()
    const header = [...root.querySelectorAll('code')].find((el) =>
      el.textContent?.includes('buz/checkout'),
    )
    expect(header, 'local group header').toBeTruthy()
    expect(root.textContent).toContain('v1.1.0')
    expect(root.textContent).toContain('v1.0.0')
  })

  it('toggles a pool version through the API', async () => {
    const root = await mountPage()
    const buttons = [...root.querySelectorAll<HTMLButtonElement>('button')]
    const enable = buttons.find((b) => b.textContent?.trim() === 'Enable')!
    expect(enable, 'staged version exposes Enable').toBeTruthy()
    enable.click()
    await flush()
    expect(stepSourcesApi.togglePoolEntry).toHaveBeenCalledWith(
      'alice',
      'oss-upload',
      '1.1.0',
      true,
    )
  })
})
