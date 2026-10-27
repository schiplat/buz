import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import PluginPool from '@/pages/PluginPool.vue'
import { pluginSourcesApi, type PoolEntry, type PoolEntryMeta } from '@/api/pluginSources'

const { dialog } = vi.hoisted(() => ({
  dialog: { alert: vi.fn(), confirm: vi.fn() },
}))

vi.mock('@/api/pluginSources', () => ({
  pluginSourcesApi: {
    listPool: vi.fn(),
    togglePoolEntry: vi.fn(),
    refreshPlugin: vi.fn(),
  },
}))
vi.mock('@/composables/useAppDialog', () => ({ useAppDialog: () => dialog }))

const entryMeta = (overrides: Partial<PoolEntryMeta> = {}): PoolEntryMeta => ({
  name: null,
  description: null,
  description_zh: null,
  category: null,
  author: null,
  publisher: null,
  license: null,
  source_repo: null,
  engine: null,
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
  requires_buz: null,
  buz_compatible: null,
  compat_error: null,
  meta: null,
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

const groupHeaders = (root: HTMLElement) =>
  [...root.querySelectorAll('code')].filter((el) =>
    (el.textContent ?? '').includes('/'),
  )

const versionSelect = (root: HTMLElement) =>
  [...root.querySelectorAll<HTMLSelectElement>('select')].find((s) =>
    [...s.options].some((o) => o.text.includes('—')),
  )!

const buttonByText = (root: HTMLElement, text: string) =>
  [...root.querySelectorAll<HTMLButtonElement>('button')].find(
    (b) => b.textContent?.trim() === text,
  )

const defaultItems = (): PoolEntry[] => [
  poolEntry({
    version: '1.0.0',
    enabled: true,
    reviewed: true,
    meta: entryMeta({
      name: 'OSS upload',
      description: 'Upload a file to Aliyun OSS.',
      description_zh: '将文件上传到阿里云 OSS。',
      category: 'artifact',
      publisher: 'official',
      engine: 'shell',
    }),
  }),
  poolEntry({
    version: '1.1.0',
    enabled: false,
    buz_compatible: true,
    meta: entryMeta({
      name: 'OSS upload',
      description: 'Upload a file to Aliyun OSS.',
      description_zh: '将文件上传到阿里云 OSS。',
      category: 'artifact',
      publisher: 'official',
      engine: 'shell',
    }),
  }),
]

beforeEach(() => {
  vi.resetAllMocks()
  dialog.alert.mockResolvedValue(undefined)
  vi.mocked(pluginSourcesApi.listPool).mockResolvedValue({ items: defaultItems() })
  vi.mocked(pluginSourcesApi.togglePoolEntry).mockResolvedValue(undefined)
  vi.mocked(pluginSourcesApi.refreshPlugin).mockResolvedValue({ items: [] })
})

afterEach(() => {
  app?.unmount()
  app = undefined
  document.body.innerHTML = ''
  vi.unstubAllGlobals()
})

describe('PluginPool', () => {
  it('lists every plugin with its uses: reference and a version dropdown', async () => {
    const root = await mountPage()
    expect(groupHeaders(root).map((el) => el.textContent)).toEqual(['uses: alice/oss-upload'])
    expect(root.textContent).toContain('OSS upload')
    const options = [...versionSelect(root).options].map((o) => o.text)
    // Options stay short (state detail lives on the line under the select,
    // where a narrow rail cannot truncate it).
    expect(options).toEqual([
      'v1.1.0 — staged',
      'v1.0.0 — enabled',
    ])
    // Review status is not surfaced for reviewed entries at all — only an
    // unreviewed selection adds a tooltip (covered by the gate tests).
    expect(buttonByText(root, 'Disable').title).toBe('')
  })

  it('describes the plugin: category, publisher, engine and the description', async () => {
    const root = await mountPage()
    expect(root.textContent).toContain('artifact')
    expect(root.textContent).toContain('Official')
    // Meta line is plain text now (pills removed): engine name only.
    expect(root.textContent).toContain('shell')
    expect(root.textContent).toContain('Upload a file to Aliyun OSS.')
  })

  it('prefers the Chinese description on a zh locale', async () => {
    vi.stubGlobal('navigator', { ...navigator, language: 'zh-CN' })
    const root = await mountPage()
    expect(root.textContent).toContain('将文件上传到阿里云 OSS。')
    expect(root.textContent).not.toContain('Upload a file to Aliyun OSS.')
  })

  it('defaults the dropdown to the enabled version and disables on demand', async () => {
    const root = await mountPage()
    // Active version (v1.0.0, enabled) is selected → the action is Disable.
    expect(versionSelect(root).value).toBe('1.0.0')
    const disable = buttonByText(root, 'Disable')!
    expect(disable).toBeTruthy()
    disable.click()
    await flush()
    expect(pluginSourcesApi.togglePoolEntry).toHaveBeenCalledWith(
      'alice',
      'oss-upload',
      '1.0.0',
      false,
    )
  })

  it('enables the version picked in the dropdown', async () => {
    const root = await mountPage()
    const select = versionSelect(root)
    select.value = '1.1.0'
    select.dispatchEvent(new Event('change'))
    await flush()
    const enable = buttonByText(root, 'Enable')!
    expect(enable).toBeTruthy()
    enable.click()
    await flush()
    expect(pluginSourcesApi.togglePoolEntry).toHaveBeenCalledWith(
      'alice',
      'oss-upload',
      '1.1.0',
      true,
    )
  })

  it('marks incompatible versions in the dropdown, strip and a disabled Enable', async () => {
    vi.mocked(pluginSourcesApi.listPool).mockResolvedValue({
      items: [
        poolEntry({
          version: '1.1.0',
          enabled: false,
          requires_buz: '>=99.0.0',
          buz_compatible: false,
          compat_error:
            "step spec 'alice/oss-upload' is invalid: requires_buz: step requires Buz >= 99.0.0 — this plugin is too new for this Manager (current 1.10.5)",
        }),
        poolEntry({ version: '1.0.0', enabled: true, reviewed: true }),
      ],
    })
    const root = await mountPage()
    // Dropdown stays short; the blocked reason appears on the state line once
    // the version is selected (see below).
    const blocked = [...versionSelect(root).options].find((o) => o.value === '1.1.0')!
    expect(blocked.text).toBe('v1.1.0 — staged')
    // …the notice states need / have / consequence without hover or click,
    // as a summary with a separate remedy.
    expect(root.textContent).toContain('v1.1.0 requires Buz >= 99.0.0, this instance runs 1.10.5')
    expect(root.textContent).toContain('Cannot be enabled on this Manager.')
    // …and selecting it leaves Enable greyed out.
    const select = versionSelect(root)
    select.value = '1.1.0'
    select.dispatchEvent(new Event('change'))
    await flush()
    // The amber note (one line) names why the selected version is unusable.
    expect(root.textContent).toContain('requires Buz >= 99.0.0')
    expect(root.textContent).toContain('Cannot be enabled on this Manager.')
    const enable = buttonByText(root, 'Enable')!
    expect(enable.disabled, 'Enable greyed for an incompatible version').toBe(true)
    enable.click()
    await flush()
    expect(pluginSourcesApi.togglePoolEntry).not.toHaveBeenCalled()
  })

  it('warns that an enabled-but-incompatible version may fail on new runs', async () => {
    vi.mocked(pluginSourcesApi.listPool).mockResolvedValue({
      items: [
        poolEntry({
          version: '1.0.0',
          enabled: true,
          requires_buz: '>=99.0.0',
          buz_compatible: false,
          compat_error:
            "step spec 'alice/oss-upload' is invalid: requires_buz: step requires Buz >= 99.0.0 — this plugin is too new for this Manager (current 1.10.5)",
        }),
      ],
    })
    const root = await mountPage()
    expect(root.textContent).toContain('v1.0.0 is enabled but cannot run on this Manager')
    expect(root.textContent).toContain('Disable it or upgrade Buz — new runs may fail.')
    // Disable stays available so the stuck version can be retired.
    const disable = buttonByText(root, 'Disable')!
    expect(disable.disabled).toBe(false)
    disable.click()
    await flush()
    expect(pluginSourcesApi.togglePoolEntry).toHaveBeenCalledWith(
      'alice',
      'oss-upload',
      '1.0.0',
      false,
    )
  })

  it('surfaces a missing source file as a plain-language strip', async () => {
    vi.mocked(pluginSourcesApi.listPool).mockResolvedValue({
      items: [
        poolEntry({
          version: '1.0.0',
          requires_buz: '>=1.0.0',
          buz_compatible: false,
          compat_error:
            'template file is listed in the source index but failed to download: template download from https://src/1.0.0.yaml failed: source returned HTTP 404',
        }),
      ],
    })
    const root = await mountPage()
    expect(root.textContent).toContain(
      'v1.0.0 is listed in the source index but failed to download',
    )
    expect(root.textContent).toContain('The file is missing on the source — publish it, then')
    // The remedy sits in the message itself: refreshing is what applies it.
    const refreshInNotice = [...root.querySelectorAll<HTMLButtonElement>('button')].find(
      (b) => b.textContent?.trim() === '⟳ Refresh',
    )
    expect(refreshInNotice, 'notice offers Refresh').toBeTruthy()
    refreshInNotice!.click()
    await flush()
    expect(pluginSourcesApi.refreshPlugin).toHaveBeenCalledWith('alice', 'oss-upload')
    // A file the source does not serve cannot be enabled — the action locks.
    expect(buttonByText(root, 'Enable').disabled, 'Enable locked for a download failure').toBe(true)
  })

  it('locks the action rail for a download failure even when the version is enabled', async () => {
    vi.mocked(pluginSourcesApi.listPool).mockResolvedValue({
      items: [
        poolEntry({
          version: '1.0.0',
          enabled: true,
          buz_compatible: false,
          compat_error:
            'template file is listed in the source index but failed to download: HTTP 404',
        }),
      ],
    })
    const root = await mountPage()
    // Enabled, so the button reads Disable — and it is locked too: there is
    // nothing to disable to safety (the entry cannot resolve at claim time).
    const action = buttonByText(root, 'Disable')!
    expect(action.disabled, 'Disable locked for a download failure').toBe(true)
    expect(action.title).toContain('The source does not serve this file')
    // Refresh stays available: re-fetching is the remedy once the source is fixed.
    const refresh = root.querySelector<HTMLButtonElement>(
      'button[aria-label="Refresh alice/oss-upload"]',
    )!
    expect(refresh.disabled).toBe(false)
  })

  it('offers one-click upgrade: enable the target and retire the old version', async () => {
    vi.mocked(pluginSourcesApi.listPool).mockResolvedValue({
      items: [
        poolEntry({ version: '1.1.0', enabled: false, buz_compatible: true }),
        poolEntry({ version: '1.0.0', enabled: true, buz_compatible: true }),
      ],
    })
    const root = await mountPage()
    const upgrade = buttonByText(root, 'Upgrade to v1.1.0')!
    expect(upgrade, 'upgrade button when a newer compatible version is staged').toBeTruthy()
    upgrade.click()
    await flush()
    expect(pluginSourcesApi.togglePoolEntry).toHaveBeenCalledTimes(2)
    expect(pluginSourcesApi.togglePoolEntry).toHaveBeenCalledWith(
      'alice',
      'oss-upload',
      '1.1.0',
      true,
    )
    expect(pluginSourcesApi.togglePoolEntry).toHaveBeenCalledWith(
      'alice',
      'oss-upload',
      '1.0.0',
      false,
    )
  })

  it('shows only Enable for a single staged version — no upgrade, no update badge', async () => {
    vi.mocked(pluginSourcesApi.listPool).mockResolvedValue({
      items: [poolEntry({ version: '1.0.0', enabled: false, buz_compatible: true })],
    })
    const root = await mountPage()
    expect(buttonByText(root, 'Enable'), 'Enable is the action for a staged version').toBeTruthy()
    expect(
      buttonByText(root, 'Upgrade to v1.0.0'),
      'nothing is enabled, so there is nothing to upgrade from',
    ).toBeUndefined()
    expect(root.textContent).not.toContain('update available')
  })

  it('hides the upgrade button when the staged newer version is incompatible', async () => {
    vi.mocked(pluginSourcesApi.listPool).mockResolvedValue({
      items: [
        poolEntry({
          version: '2.0.0',
          enabled: false,
          buz_compatible: false,
          compat_error:
            "step spec 'alice/oss-upload' is invalid: requires_buz: step requires Buz >= 99.0.0 — too new",
        }),
        poolEntry({ version: '1.0.0', enabled: true, buz_compatible: true }),
      ],
    })
    const root = await mountPage()
    expect(buttonByText(root, 'Upgrade to v2.0.0')).toBeUndefined()
    // No separate "update available" badge: the amber note explains why the
    // newer version cannot be used, which is the actionable part.
    expect(root.textContent).not.toContain('update available')
    expect(root.textContent).toContain('requires Buz >= 99.0.0')
  })

  it('searches plugins by name, description and uses reference', async () => {
    const root = await mountPage()
    const input = root.querySelector<HTMLInputElement>('input[type="search"]')!
    input.value = 'oss'
    input.dispatchEvent(new Event('input'))
    await flush()
    expect(groupHeaders(root)).toHaveLength(1)

    input.value = 'no-such-thing'
    input.dispatchEvent(new Event('input'))
    await flush()
    expect(groupHeaders(root)).toHaveLength(0)
    expect(root.textContent).toContain('No plugins match your search')
  })

  it('paginates plugins and keeps the page across a reload that only flips states', async () => {
    vi.mocked(pluginSourcesApi.listPool).mockResolvedValue({
      items: Array.from({ length: 25 }, (_, i) =>
        poolEntry({ step_id: `step-${i + 1}` }),
      ),
    })
    const root = await mountPage()
    expect(groupHeaders(root)).toHaveLength(20)

    const next = [...root.querySelectorAll<HTMLButtonElement>('button')].find(
      (b) => b.textContent?.trim() === 'Next',
    )!
    next.click()
    await flush()
    expect(groupHeaders(root)).toHaveLength(5)
    expect(root.textContent).toContain('21–25 of 25')
    expect(root.textContent).toContain('2 / 2')

    // Toggle the selected version on page 2: the reload must NOT jump back.
    vi.mocked(pluginSourcesApi.listPool).mockClear()
    const enable = buttonByText(root, 'Enable')!
    enable.click()
    await flush()
    expect(groupHeaders(root)).toHaveLength(5)

    // Shrinking the page size refits the window (page-size select offers 10..100).
    const select = [...root.querySelectorAll<HTMLSelectElement>('select')].find((s) =>
      [...s.options].some((o) => o.value === '10' && [...s.options].some((o2) => o2.value === '50')),
    )!
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
    expect(pluginSourcesApi.refreshPlugin).toHaveBeenCalledWith('alice', 'oss-upload')
  })

  it('copies the uses reference via the identity-line copy button', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined)
    vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText } })
    const root = await mountPage()
    const copy = [...root.querySelectorAll<HTMLButtonElement>('button')].find(
      (b) => b.getAttribute('aria-label') === 'Copy alice/oss-upload',
    )!
    expect(copy, 'copy button next to the uses chip').toBeTruthy()
    copy.click()
    await flush()
    expect(writeText).toHaveBeenCalledWith('alice/oss-upload')
    expect(copy.querySelector('svg.text-green-600')).toBeTruthy()
  })
})
