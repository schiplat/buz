import { computed, ref, toValue, watch, type MaybeRefOrGetter } from 'vue'

export const PAGE_SIZE_OPTIONS = [10, 20, 50, 100] as const

export function useClientPagination<T>(
  source: MaybeRefOrGetter<T[]>,
  options?: {
    initialPageSize?: number
    /** When given, a change resets the page only when THIS value changes —
     *  a mere refresh of `source` (same items, new array) keeps the page.
     *  Pass a signature of the item set, e.g. the joined ids. */
    resetKey?: MaybeRefOrGetter<unknown>
  },
) {
  const page = ref(1)
  const pageSize = ref(options?.initialPageSize ?? 20)

  const total = computed(() => toValue(source).length)
  const pageCount = computed(() => Math.max(1, Math.ceil(total.value / pageSize.value) || 1))

  const pageItems = computed(() => {
    const list = toValue(source)
    const start = (page.value - 1) * pageSize.value
    return list.slice(start, start + pageSize.value)
  })

  const rangeLabel = computed(() => {
    if (total.value === 0) return '0 of 0'
    const start = (page.value - 1) * pageSize.value + 1
    const end = Math.min(page.value * pageSize.value, total.value)
    return `${start}–${end} of ${total.value}`
  })

  if (options?.resetKey !== undefined) {
    watch(
      () => toValue(options.resetKey!),
      () => { page.value = 1 },
    )
  } else {
    watch(
      () => toValue(source),
      () => { page.value = 1 },
    )
  }

  watch([total, pageSize], () => {
    if (page.value > pageCount.value) page.value = pageCount.value
    if (page.value < 1) page.value = 1
  })

  return { page, pageSize, pageCount, total, pageItems, rangeLabel }
}
