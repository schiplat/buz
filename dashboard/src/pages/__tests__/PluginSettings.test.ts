import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createApp, nextTick, type App } from 'vue'
import PluginSettings from '@/pages/PluginSettings.vue'
import { adminSettingsApi, type PlatformSetting } from '@/api/adminSettings'

const { dialog, auth } = vi.hoisted(() => ({
  dialog: { alert: vi.fn(), confirm: vi.fn() },
  auth: { isPlatformAdmin: true },
}))

vi.mock('@/stores/auth', () => ({ useAuthStore: () => auth }))
vi.mock('@/composables/useAppDialog', () => ({ useAppDialog: () => dialog }))
vi.mock('@/api/adminSettings', () => ({ adminSettingsApi: { list: vi.fn(), update: vi.fn(), reset: vi.fn() } }))
vi.mock('vue-router', () => ({ useRouter: () => ({ replace: vi.fn() }) }))

const setting = (overrides: Partial<PlatformSetting>): PlatformSetting => ({
  key: 'marketplace_url',
  kind: 'url',
  value: '',
  default: '',
  overridden: false,
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
  app = createApp(PluginSettings)
  app.mount(root)
  await flush()
  return root
}

beforeEach(() => {
  vi.resetAllMocks()
  dialog.alert.mockResolvedValue(undefined)
  vi.mocked(adminSettingsApi.list).mockResolvedValue({
    items: [
      setting({ key: 'marketplace_url', kind: 'url', value: 'https://plugins.example.com', default: '' }),
      setting({ key: 'step_review_required', kind: 'bool', value: 'false', default: 'false' }),
      setting({ key: 'step_index_local_dir', kind: 'path', value: '/srv/steps', default: '/srv/steps' }),
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

describe('PluginSettings', () => {
  it('renders every setting with its effective value', async () => {
    const root = await mountPage()
    const inputs = [...root.querySelectorAll<HTMLInputElement>('input[type="url"], input[type="text"]')]
    expect(inputs.map((i) => i.value)).toEqual([
      'https://plugins.example.com',
      '/srv/steps',
    ])
    expect(document.body.textContent).toContain('Step review gate')
  })

  it('saves an edited draft through PUT and refreshes the effective value', async () => {
    const root = await mountPage()
    const input = root.querySelector<HTMLInputElement>('input[type="url"]')!
    input.value = 'https://mirror.corp/plugins'
    input.dispatchEvent(new Event('input', { bubbles: true }))
    await nextTick()

    const save = [...root.querySelectorAll<HTMLButtonElement>('button')].find((b) => b.textContent?.trim() === 'Save')!
    save.click()
    await flush()

    expect(adminSettingsApi.update).toHaveBeenCalledWith('marketplace_url', 'https://mirror.corp/plugins')
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

  it('resets an override back to the manager.toml default', async () => {
    vi.mocked(adminSettingsApi.list).mockResolvedValue({
      items: [
        setting({
          key: 'marketplace_url',
          kind: 'url',
          value: 'https://mirror.corp/plugins',
          default: '',
          overridden: true,
        }),
      ],
    })
    const root = await mountPage()
    const urlSection = root.querySelector<HTMLInputElement>('input[type="url"]')!
    const reset = [...root.querySelectorAll<HTMLButtonElement>('button')]
      .find((b) => b.textContent?.includes('Reset'))!
    reset.click()
    await flush()

    expect(adminSettingsApi.reset).toHaveBeenCalledWith('marketplace_url')
    expect(urlSection.value).toBe('')
  })
})
