import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import PluginPool from '@/pages/PluginPool.vue'
import { stepSourcesApi, type PoolEntry } from '@/api/stepSources'

const { dialog } = vi.hoisted(() => ({
  dialog: { alert: vi.fn(), confirm: vi.fn() },
}))

vi.mock('@/api/stepSources', () => ({
  stepSourcesApi: {
    listPool: vi.fn(),
    togglePoolEntry: vi.fn(),
    refreshPlugin: vi.fn(),
  },
}))
vi.mock('@/composables/useAppDialog', () => ({ useAppDialog: () => dialog }))

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
  requires_buz: null,
  buz_compatible: null,
  compat_error: null,
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
  app = createApp(PluginPool)
  app.mount(root)
  await flush()
  return root
}

/** Version chips are buttons whose text is the version ("v1.1.0"); the only
 *  other buttons are Prev/Next/page numbers/refresh (icon-only). */
const chip = (root: HTMLElement, version: string) =>
  [...root.querySelectorAll<HTMLButtonElement>('button')].find((b) =>
    b.textContent?.trim() === version,
  )

const groupHeaders = (root: HTMLElement) =>
  [...root.querySelectorAll('code')].filter((el) =>
    (el.textContent ?? '').includes('/'),
  )

beforeEach(() => {
  vi.resetAllMocks()
  dialog.alert.mockResolvedValue(undefined)
  vi.mocked(stepSourcesApi.listPool).mockResolvedValue({
    items: [
      poolEntry({ version: '1.0.0', enabled: true, reviewed: true }),
      poolEntry({ version: '1.1.0', enabled: false }),
    ],
  })
  vi.mocked(stepSourcesApi.togglePoolEntry).mockResolvedValue(undefined)
  vi.mocked(stepSourcesApi.refreshPlugin).mockResolvedValue({ items: [] })
})

afterEach(() => {
  app?.unmount()
  app = undefined
  document.body.innerHTML = ''
})

describe('PluginPool', () => {
  it('lists every plugin with its versions as chips, newest first', async () => {
    const root = await mountPage()
    expect(groupHeaders(root).map((el) => el.textContent)).toEqual(['alice/oss-upload'])
    const versions = [...root.querySelectorAll<HTMLButtonElement>('button')]
      .map((b) => b.textContent?.trim())
      .filter((t) => t?.startsWith('v'))
    expect(versions).toEqual(['v1.1.0', 'v1.0.0'])
  })

  it('colors chips by state and keeps the update-available verdict per group', async () => {
    const root = await mountPage()
    const enabled = chip(root, 'v1.0.0')!
    expect(enabled.className).toContain('bg-green-500/10')
    expect(enabled.querySelector('svg'), 'reviewed check icon').toBeTruthy()
    const staged = chip(root, 'v1.1.0')!
    expect(staged.className).toContain('bg-muted/30')
    const badges = [...root.querySelectorAll('span')].map((s) => s.textContent)
    expect(badges).toContain('update available')
  })

  it('toggles a pool version through the API on chip click', async () => {
    const root = await mountPage()
    chip(root, 'v1.1.0')!.click()
    await flush()
    expect(stepSourcesApi.togglePoolEntry).toHaveBeenCalledWith(
      'alice',
      'oss-upload',
      '1.1.0',
      true,
    )
  })

  it('blocks enabling an incompatible version and explains why instead', async () => {
    vi.mocked(stepSourcesApi.listPool).mockResolvedValue({
      items: [
        poolEntry({
          version: '1.1.0',
          enabled: false,
          requires_buz: '>=99.0.0',
          buz_compatible: false,
          compat_error:
            "step spec 'alice/oss-upload' is invalid: requires_buz: step requires Buz >= 99.0.0 — this plugin is too new for this Manager (current 1.10.4)",
        }),
      ],
    })
    const root = await mountPage()
    const stuck = chip(root, 'v1.1.0')!
    expect(stuck.className).toContain('bg-red-500/10')
    expect(stuck.querySelector('svg'), 'incompatibility warning icon').toBeTruthy()

    stuck.click()
    await flush()
    expect(dialog.alert).toHaveBeenCalledWith(expect.stringContaining('>= 99.0.0'))
    expect(stepSourcesApi.togglePoolEntry).not.toHaveBeenCalled()
  })

  it('keeps an enabled incompatible version clickable so it can be disabled', async () => {
    vi.mocked(stepSourcesApi.listPool).mockResolvedValue({
      items: [
        poolEntry({
          version: '1.0.0',
          enabled: true,
          requires_buz: '>=99.0.0',
          buz_compatible: false,
          compat_error: 'too new',
        }),
      ],
    })
    const root = await mountPage()
    expect(chip(root, 'v1.0.0')!.className).toContain('bg-red-500/10')
    chip(root, 'v1.0.0')!.click()
    await flush()
    expect(stepSourcesApi.togglePoolEntry).toHaveBeenCalledWith(
      'alice',
      'oss-upload',
      '1.0.0',
      false,
    )
    expect(dialog.alert).not.toHaveBeenCalled()
  })

  it('paginates plugins and keeps the page across a reload that only flips states', async () => {
    vi.mocked(stepSourcesApi.listPool).mockResolvedValue({
      items: Array.from({ length: 25 }, (_, i) =>
        poolEntry({ step_id: `step-${i + 1}` }),
      ),
    })
    const root = await mountPage()
    expect(groupHeaders(root)).toHaveLength(20)

    // Prev/Next + total pages: 25 plugins on 20/page → 2 pages.
    const next = [...root.querySelectorAll<HTMLButtonElement>('button')].find(
      (b) => b.textContent?.trim() === 'Next',
    )!
    next.click()
    await flush()
    expect(groupHeaders(root)).toHaveLength(5)
    expect(root.textContent).toContain('21–25 of 25')
    expect(root.textContent).toContain('2 / 2')

    // Toggle one chip on page 2: the reload must NOT jump back to page 1.
    vi.mocked(stepSourcesApi.listPool).mockClear()
    chip(root, 'v1.0.0')!.click()
    await flush()
    expect(groupHeaders(root)).toHaveLength(5)

    // Shrinking the page size refits the window.
    const select = root.querySelector<HTMLSelectElement>('select')!
    select.value = '10'
    select.dispatchEvent(new Event('change'))
    await flush()
    expect(groupHeaders(root)).toHaveLength(10)
  })

  it('refreshes one plugin through the group icon button', async () => {
    const root = await mountPage()
    const refresh = [...root.querySelectorAll<HTMLButtonElement>('button')].find(
      (b) => b.getAttribute('aria-label') === 'Refresh alice/oss-upload',
    )!
    expect(refresh, 'icon-only refresh button').toBeTruthy()
    refresh.click()
    await flush()
    expect(stepSourcesApi.refreshPlugin).toHaveBeenCalledWith('alice', 'oss-upload')
  })
})
