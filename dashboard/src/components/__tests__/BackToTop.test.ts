import { describe, expect, it, afterEach, vi } from 'vitest'
import { createApp, nextTick } from 'vue'
import BackToTop from '@/components/BackToTop.vue'

function mountBackToTop() {
  const el = document.createElement('div')
  document.body.appendChild(el)
  const app = createApp(BackToTop)
  app.mount(el)
  return {
    el,
    unmount: () => {
      app.unmount()
      el.remove()
    },
  }
}

/** jsdom has no scroll mechanics — stub scrollY + fire the scroll event. */
function setScrollY(value: number) {
  Object.defineProperty(window, 'scrollY', { value, configurable: true, writable: true })
  window.dispatchEvent(new Event('scroll'))
}

afterEach(() => {
  vi.restoreAllMocks()
})

describe('BackToTop', () => {
  it('is hidden at the top of the page', async () => {
    setScrollY(0)
    const { el, unmount } = mountBackToTop()
    await nextTick()
    expect(el.querySelector('button')).toBeNull()
    unmount()
  })

  it('appears after scrolling past the threshold', async () => {
    setScrollY(500)
    const { el, unmount } = mountBackToTop()
    await nextTick()
    expect(el.querySelector('button')).not.toBeNull()
    unmount()
  })

  it('hides again when scrolled back up', async () => {
    setScrollY(500)
    const { el, unmount } = mountBackToTop()
    await nextTick()
    expect(el.querySelector('button')).not.toBeNull()
    setScrollY(10)
    await nextTick()
    expect(el.querySelector('button')).toBeNull()
    unmount()
  })

  it('scrolls the window to the top on click', async () => {
    setScrollY(500)
    const scrollTo = vi.fn()
    window.scrollTo = scrollTo
    const { el, unmount } = mountBackToTop()
    await nextTick()
    ;(el.querySelector('button') as HTMLButtonElement).click()
    expect(scrollTo).toHaveBeenCalledWith({ top: 0, behavior: 'smooth' })
    unmount()
  })
})
