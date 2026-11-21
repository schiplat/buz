import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { createPinia, setActivePinia, type Pinia } from 'pinia'
import { createApp, h, nextTick, type App } from 'vue'
import BrandMark from '@/components/brand/BrandMark.vue'
import { useBrandingStore } from '@/stores/branding'
import type { BrandingInfo } from '@/api/branding'

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
    note: 'Square icon mark.',
  },
})

let pinia: Pinia
let app: App | undefined
let host: HTMLDivElement | undefined

function mountMark() {
  host = document.createElement('div')
  document.body.appendChild(host)
  app = createApp({ render: () => h(BrandMark, { size: 'md' }) })
  app.use(pinia)
  app.mount(host)
}

beforeEach(() => {
  pinia = createPinia()
  setActivePinia(pinia)
})

afterEach(() => {
  app?.unmount()
  host?.remove()
  app = undefined
  host = undefined
})

describe('BrandMark', () => {
  it('renders the built-in Bz mark by default', async () => {
    useBrandingStore(pinia).info = baseInfo()
    mountMark()
    await nextTick()

    expect(host!.textContent).toContain('Bz')
    expect(host!.querySelector('img')).toBeNull()
  })

  it('renders the uploaded logo with a cache-busting version', async () => {
    useBrandingStore(pinia).info = {
      ...baseInfo(),
      custom: true,
      mime: 'image/png',
      etag: 'abc123',
    }
    mountMark()
    await nextTick()

    const img = host!.querySelector('img')
    expect(img).toBeTruthy()
    expect(img!.getAttribute('src')).toBe('/api/v1/branding/logo?v=abc123')
    expect(host!.textContent).not.toContain('Bz')
  })
})
