import { afterEach, describe, expect, it } from 'vitest'
import { createApp, defineComponent, h, ref, type App } from 'vue'
import TablePagination from '@/components/ui/TablePagination.vue'

let app: App | undefined

afterEach(() => {
  app?.unmount()
  app = undefined
  document.body.innerHTML = ''
})

/** Mount the shared pagination through a tiny host so v-model page updates
 *  are observable, like every list page uses it. */
async function mountPagination(pageCount: number, initialPage = 1) {
  const page = ref(initialPage)
  const Host = defineComponent({
    setup() {
      return () =>
        h(TablePagination, {
          page: page.value,
          'onUpdate:page': (value: number) => (page.value = value),
          pageSize: 20,
          'onUpdate:pageSize': () => {},
          total: pageCount * 20,
          pageCount,
          rangeLabel: `1–20 of ${pageCount * 20}`,
        })
    },
  })
  const root = document.createElement('div')
  document.body.appendChild(root)
  app = createApp(Host)
  app.mount(root)
  await Promise.resolve()
  return { root, page }
}

const pageButtons = (root: HTMLElement) =>
  [...root.querySelectorAll<HTMLButtonElement>('button')]
    .filter((b) => /^\d+$/.test(b.textContent?.trim() ?? ''))
    .map((b) => b.textContent!.trim())

describe('TablePagination', () => {
  it('renders a compact full list when pages fit in the window', async () => {
    const { root } = await mountPagination(5)
    expect(pageButtons(root)).toEqual(['1', '2', '3', '4', '5'])
    expect(root.textContent).not.toContain('…')
  })

  it('shows the nearest pages with ellipses in the middle', async () => {
    const { root } = await mountPagination(12, 6)
    expect(pageButtons(root)).toEqual(['1', '5', '6', '7', '12'])
    expect(root.textContent).toContain('…')
    expect(root.querySelector('[aria-current="page"]')?.textContent?.trim()).toBe('6')
  })

  it('pins the window to the edges without a leading/trailing ellipse', async () => {
    const first = await mountPagination(12, 1)
    expect(pageButtons(first.root)).toEqual(['1', '2', '3', '12'])
    const last = await mountPagination(12, 12)
    expect(pageButtons(last.root)).toEqual(['1', '10', '11', '12'])
  })

  it('navigates on number, Prev and Next clicks and clamps at the edges', async () => {
    const { root, page } = await mountPagination(12, 1)
    const byLabel = (label: string) =>
      [...root.querySelectorAll<HTMLButtonElement>('button')].find(
        (b) => b.textContent?.trim() === label,
      )!
    byLabel('12').click()
    await Promise.resolve()
    expect(page.value).toBe(12)
    byLabel('Prev').click()
    await Promise.resolve()
    expect(page.value).toBe(11)
    byLabel('Next').click()
    await Promise.resolve()
    expect(page.value).toBe(12)
    expect(byLabel('Next').disabled, 'Next disabled on the last page').toBe(true)
    expect(byLabel('Prev').disabled).toBe(false)

    const only = await mountPagination(1)
    expect(pageButtons(only.root), 'single page renders no numbers').toEqual([])
    expect(only.root.textContent).toContain('1 / 1')
  })
})
