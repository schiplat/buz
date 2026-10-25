import { describe, expect, it, beforeEach, vi } from 'vitest'
import { createApp, nextTick } from 'vue'
import ReviewGateBanner from '@/components/ReviewGateBanner.vue'

const getStepReviewGate = vi.fn()

vi.mock('@/api/health', () => ({
  getStepReviewGate: (...args: unknown[]) => getStepReviewGate(...args),
}))
// The banner links to the settings page; the standalone mount has no router.
vi.mock('vue-router', () => ({ RouterLink: { template: '<a><slot /></a>' } }))

function mountBanner() {
  const el = document.createElement('div')
  document.body.appendChild(el)
  const app = createApp(ReviewGateBanner)
  // The banner links to the settings page; the standalone mount has no
  // router, so provide an inert stub.
  app.component('RouterLink', { template: '<a><slot /></a>' })
  app.mount(el)
  return {
    el,
    unmount: () => {
      app.unmount()
      el.remove()
    },
  }
}

async function settle() {
  await nextTick()
  await nextTick()
}

beforeEach(() => {
  getStepReviewGate.mockReset()
})

describe('ReviewGateBanner', () => {
  it('shows when the manager reports no reviewed manifest configured', async () => {
    getStepReviewGate.mockResolvedValue({ manifest_configured: false, require_reviewed: false })
    const { el, unmount } = mountBanner()
    await settle()
    expect(el.querySelector('[data-testid="review-gate-banner"]')).not.toBeNull()
    unmount()
  })

  it('shows an ERROR banner for the dangerous combo: strict gate with no manifest', async () => {
    // require_reviewed=true with no manifest rejects every template — the
    // operator must see this immediately, not an advisory.
    getStepReviewGate.mockResolvedValue({ manifest_configured: false, require_reviewed: true })
    const { el, unmount } = mountBanner()
    await settle()
    const banner = el.querySelector('[data-testid="review-gate-banner"]')
    expect(banner).not.toBeNull()
    expect(banner!.textContent).toContain('every template will be rejected')
    expect(banner!.textContent).toContain('Plugins →')
    expect(banner!.textContent).not.toContain('manager.toml')
    unmount()
  })

  it('stays silent when the gate is configured', async () => {
    getStepReviewGate.mockResolvedValue({ manifest_configured: true, require_reviewed: false })
    const { el, unmount } = mountBanner()
    await settle()
    expect(el.querySelector('[data-testid="review-gate-banner"]')).toBeNull()
    unmount()
  })

  it('stays silent when the probe fails or the field is absent', async () => {
    getStepReviewGate.mockResolvedValue(null)
    const { el, unmount } = mountBanner()
    await settle()
    expect(el.querySelector('[data-testid="review-gate-banner"]')).toBeNull()
    unmount()
  })

  it('hides for the session once dismissed', async () => {
    getStepReviewGate.mockResolvedValue({ manifest_configured: false, require_reviewed: false })
    const { el, unmount } = mountBanner()
    await settle()
    const banner = el.querySelector('[data-testid="review-gate-banner"]')
    expect(banner).not.toBeNull()
    ;(banner!.querySelector('button') as HTMLButtonElement).click()
    await nextTick()
    expect(el.querySelector('[data-testid="review-gate-banner"]')).toBeNull()
    unmount()
  })
})
