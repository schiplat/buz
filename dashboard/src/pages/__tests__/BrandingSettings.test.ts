import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia, type Pinia } from 'pinia'
import { createApp, nextTick, type App } from 'vue'
import BrandingSettings from '@/pages/BrandingSettings.vue'
import { brandingApi, type BrandingInfo } from '@/api/branding'

vi.mock('@/api/branding', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/api/branding')>()
  return {
    ...actual,
    brandingApi: { get: vi.fn(), uploadLogo: vi.fn(), reset: vi.fn() },
  }
})

const baseInfo = (): BrandingInfo => ({
  custom: false,
  logo_url: '/api/v1/branding/logo',
  mime: null,
  size_bytes: null,
  updated_by: null,
  updated_at: null,
  etag: null,
  recommendations: {
    formats: ['SVG', 'PNG', 'WebP', 'JPEG'],
    preferred_format: 'SVG or PNG',
    dimensions: '512 × 512 px (square)',
    min_dimensions: '128 × 128 px',
    max_bytes: 524288,
    transparent_background: true,
    safe_margin: 'at least 10% on every side',
    note: 'Square icon mark — shown at 32 px in the sidebar and 40 px on sign-in pages.',
  },
})

const customInfo = (): BrandingInfo => ({
  ...baseInfo(),
  custom: true,
  mime: 'image/png',
  size_bytes: 1024,
  updated_by: 'boss',
  updated_at: '2026-11-21 10:00:00',
  etag: 'abc123',
})

let pinia: Pinia
let app: App | undefined
let host: HTMLDivElement | undefined

async function mountPage() {
  pinia = createPinia()
  setActivePinia(pinia)
  host = document.createElement('div')
  document.body.appendChild(host)
  app = createApp(BrandingSettings)
  app.use(pinia)
  app.mount(host)
  await nextTick()
  await new Promise((resolve) => setTimeout(resolve, 0))
}

function buttonByText(text: string): HTMLButtonElement | undefined {
  return [...host!.querySelectorAll('button')].find((button) =>
    button.textContent?.includes(text),
  )
}

async function pickFile(file: File) {
  const input = host!.querySelector('input[type="file"]') as HTMLInputElement
  Object.defineProperty(input, 'files', { value: [file], configurable: true })
  input.dispatchEvent(new Event('change'))
}

beforeEach(() => {
  vi.mocked(brandingApi.get).mockResolvedValue(baseInfo())
  vi.mocked(brandingApi.uploadLogo).mockResolvedValue(customInfo())
  vi.mocked(brandingApi.reset).mockResolvedValue(baseInfo())
})

afterEach(() => {
  app?.unmount()
  host?.remove()
  app = undefined
  host = undefined
  vi.clearAllMocks()
})

describe('BrandingSettings', () => {
  it('shows the built-in mark, preview caption and recommended spec', async () => {
    await mountPage()

    expect(brandingApi.get).toHaveBeenCalled()
    expect(host!.textContent).toContain('Sidebar logo')
    expect(host!.textContent).toContain('Built-in mark')
    expect(host!.textContent).toContain('Display size · 32 px')
    expect(host!.textContent).toContain('512 × 512 px square')
    expect(host!.textContent).toContain('128 × 128 px')
    expect(host!.textContent).toContain('512 KB')
    expect(host!.textContent).toContain('at least 10% on every side')
    expect(buttonByText('Upload logo')).toBeTruthy()
    // Reset is always present; it is disabled while the built-in mark shows.
    expect(buttonByText('Reset to default')!.disabled).toBe(true)
  })

  it('uploads a picked file and applies it immediately', async () => {
    await mountPage()

    await pickFile(new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], 'logo.png', { type: 'image/png' }))

    await vi.waitFor(() => expect(brandingApi.uploadLogo).toHaveBeenCalled())
    const [contentType, data] = vi.mocked(brandingApi.uploadLogo).mock.calls[0]
    expect(contentType).toBe('image/png')
    expect(data.length).toBeGreaterThan(0)

    await vi.waitFor(() =>
      expect(host!.textContent).toContain('Logo updated — every operator sees it now.'),
    )
    expect(host!.textContent).toContain('Custom logo')
    expect(buttonByText('Reset to default')!.disabled).toBe(false)
  })

  it('rejects an oversized file before calling the API', async () => {
    await mountPage()

    const tooBig = new File([new Uint8Array(600 * 1024)], 'big.png', { type: 'image/png' })
    await pickFile(tooBig)

    expect(brandingApi.uploadLogo).not.toHaveBeenCalled()
    expect(host!.textContent).toContain('the maximum is 512 KB')
  })

  it('rejects an unsupported type before calling the API', async () => {
    await mountPage()

    await pickFile(new File(['gif'], 'logo.gif', { type: 'image/gif' }))

    expect(brandingApi.uploadLogo).not.toHaveBeenCalled()
    expect(host!.textContent).toContain('Unsupported image type')
  })

  it('resets to the built-in mark immediately', async () => {
    vi.mocked(brandingApi.get).mockResolvedValue(customInfo())
    await mountPage()
    expect(host!.textContent).toContain('Custom logo')
    expect(buttonByText('Reset to default')!.disabled).toBe(false)

    buttonByText('Reset to default')!.click()
    await vi.waitFor(() => expect(brandingApi.reset).toHaveBeenCalled())

    await vi.waitFor(() => expect(host!.textContent).toContain('Back to the built-in mark.'))
    expect(host!.textContent).toContain('Built-in mark')
    expect(buttonByText('Reset to default')!.disabled).toBe(true)
  })
})
