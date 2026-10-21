import { describe, expect, it, beforeEach, vi } from 'vitest'
import { createApp, nextTick } from 'vue'
import ReviewGateBanner from '@/components/ReviewGateBanner.vue'

const getStepReviewGate = vi.fn()

vi.mock('@/api/health', () => ({
  getStepReviewGate: (...args: unknown[]) => getStepReviewGate(...args),
}))

function mountBanner() {
  const el = document.createElement('div')
  document.body.appendChild(el)
  const app = createApp(ReviewGateBanner)
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
