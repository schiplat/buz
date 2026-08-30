import { computed, ref, toValue, type MaybeRefOrGetter } from 'vue'

export type SortDir = 'asc' | 'desc'

function cmp(a: unknown, b: unknown): number {
  if (a == null && b == null) return 0
  if (a == null) return -1
  if (b == null) return 1
  if (typeof a === 'number' && typeof b === 'number') {
    return a === b ? 0 : a < b ? -1 : 1
  }
  if (typeof a === 'boolean' && typeof b === 'boolean') {
    return a === b ? 0 : a ? 1 : -1
  }
  return String(a).localeCompare(String(b), undefined, {
    numeric: true,
    sensitivity: 'base',
  })
}

export function useClientSort<T>(
  source: MaybeRefOrGetter<T[]>,
  options: {
    initialKey: string
    initialDir?: SortDir
    getValue: (row: T, key: string) => unknown
  },
) {
  const sortKey = ref(options.initialKey)
  const sortDir = ref<SortDir>(options.initialDir ?? 'asc')

  const sorted = computed(() => {
    const list = [...toValue(source)]
    const key = sortKey.value
    const dir = sortDir.value === 'asc' ? 1 : -1
    list.sort((a, b) => dir * cmp(options.getValue(a, key), options.getValue(b, key)))
    return list
  })

  function toggleSort(key: string) {
    if (sortKey.value === key) {
      sortDir.value = sortDir.value === 'asc' ? 'desc' : 'asc'
    } else {
      sortKey.value = key
      sortDir.value = 'asc'
    }
  }

  /**
   * Icon-only sort glyph for table headers.
   * Active column: ↑ (asc) / ↓ (desc). Inactive: ↕.
   * Never returns the words "asc" / "desc".
   */
  function sortIndicator(key: string): '↑' | '↓' | '↕' {
    if (sortKey.value !== key) return '↕'
    return sortDir.value === 'asc' ? '↑' : '↓'
  }

  return { sortKey, sortDir, sorted, toggleSort, sortIndicator }
}
