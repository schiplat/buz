import { computed, ref, toValue, watch, type MaybeRefOrGetter } from 'vue'

export const PAGE_SIZE_OPTIONS = [10, 20, 50, 100] as const

export function useClientPagination<T>(
  source: MaybeRefOrGetter<T[]>,
  options?: { initialPageSize?: number },
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

  watch(
    () => toValue(source),
    () => { page.value = 1 },
  )

  watch([total, pageSize], () => {
    if (page.value > pageCount.value) page.value = pageCount.value
    if (page.value < 1) page.value = 1
  })

  return { page, pageSize, pageCount, total, pageItems, rangeLabel }
}
