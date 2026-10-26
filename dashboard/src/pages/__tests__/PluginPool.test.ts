import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import PluginPool from '@/pages/PluginPool.vue'
import { stepSourcesApi, type PoolEntry, type PoolEntryMeta } from '@/api/stepSources'

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
  vi.mocked(stepSourcesApi.listPool).mockResolvedValue({ items: defaultItems() })
  vi.mocked(stepSourcesApi.togglePoolEntry).mockResolvedValue(undefined)
  vi.mocked(stepSourcesApi.refreshPlugin).mockResolvedValue({ items: [] })
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
    expect(options).toEqual([
      'v1.1.0 — staged',
      'v1.0.0 — enabled · reviewed',
    ])
  })

  it('describes the plugin: category, publisher, engine and the description', async () => {
    const root = await mountPage()
    expect(root.textContent).toContain('artifact')
    expect(root.textContent).toContain('Official')
    expect(root.textContent).toContain('shell engine')
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
    expect(stepSourcesApi.togglePoolEntry).toHaveBeenCalledWith(
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
    expect(stepSourcesApi.togglePoolEntry).toHaveBeenCalledWith(
      'alice',
      'oss-upload',
      '1.1.0',
      true,
    )
  })

  it('marks incompatible versions in the dropdown, strip and a disabled Enable', async () => {
    vi.mocked(stepSourcesApi.listPool).mockResolvedValue({
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
    // Dropdown labels the blocked version…
    const blocked = [...versionSelect(root).options].find((o) => o.value === '1.1.0')!
    expect(blocked.text).toContain('blocked (needs Buz >= 99.0.0)')
    // …the strip states need / have / consequence without hover or click…
    expect(root.textContent).toContain(
      'v1.1.0 requires Buz >= 99.0.0, but this instance runs 1.10.5 — it cannot be enabled here.',
    )
    // …and selecting it leaves Enable greyed out.
    const select = versionSelect(root)
    select.value = '1.1.0'
    select.dispatchEvent(new Event('change'))
    await flush()
    const enable = buttonByText(root, 'Enable')!
    expect(enable.disabled, 'Enable greyed for an incompatible version').toBe(true)
    enable.click()
    await flush()
    expect(stepSourcesApi.togglePoolEntry).not.toHaveBeenCalled()
  })

  it('warns that an enabled-but-incompatible version may fail on new runs', async () => {
    vi.mocked(stepSourcesApi.listPool).mockResolvedValue({
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
    expect(root.textContent).toContain('v1.0.0 remains enabled and may fail on new runs')
    // Disable stays available so the stuck version can be retired.
    const disable = buttonByText(root, 'Disable')!
    expect(disable.disabled).toBe(false)
    disable.click()
    await flush()
    expect(stepSourcesApi.togglePoolEntry).toHaveBeenCalledWith(
      'alice',
      'oss-upload',
      '1.0.0',
      false,
    )
  })

  it('surfaces a missing source file as a plain-language strip', async () => {
    vi.mocked(stepSourcesApi.listPool).mockResolvedValue({
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
    expect(root.textContent).toContain('the file is missing on the source')
  })

  it('offers one-click upgrade: enable the target and retire the old version', async () => {
    vi.mocked(stepSourcesApi.listPool).mockResolvedValue({
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
    expect(stepSourcesApi.togglePoolEntry).toHaveBeenCalledTimes(2)
    expect(stepSourcesApi.togglePoolEntry).toHaveBeenCalledWith(
      'alice',
      'oss-upload',
      '1.1.0',
      true,
    )
    expect(stepSourcesApi.togglePoolEntry).toHaveBeenCalledWith(
      'alice',
      'oss-upload',
      '1.0.0',
      false,
    )
  })

  it('hides the upgrade button when the staged newer version is incompatible', async () => {
    vi.mocked(stepSourcesApi.listPool).mockResolvedValue({
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
    expect(root.textContent).toContain('update available')
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
    vi.mocked(stepSourcesApi.listPool).mockResolvedValue({
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
    vi.mocked(stepSourcesApi.listPool).mockClear()
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
    expect(stepSourcesApi.refreshPlugin).toHaveBeenCalledWith('alice', 'oss-upload')
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
