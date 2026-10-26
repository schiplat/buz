<script setup lang="ts">
import { computed } from 'vue'
import { PAGE_SIZE_OPTIONS } from '@/composables/useClientPagination'

const page = defineModel<number>('page', { required: true })
const pageSize = defineModel<number>('pageSize', { required: true })

const props = defineProps<{
  total: number
  pageCount: number
  rangeLabel: string
}>()

interface PageItem {
  key: string
  label: string
  page: number | null
}

/** Numbered page window around the current page: first · … · p-1 p p+1 · …
 *  · last. Near an edge the window pads toward the middle so the ellipsis
 *  never hugs the first/last page. */
const pageItems = computed<PageItem[]>(() => {
  const count = props.pageCount
  if (count <= 1) return []
  const current = Math.min(Math.max(page.value, 1), count)
  const last = count
  const range = (from: number, to: number): PageItem[] =>
    Array.from({ length: Math.max(0, to - from + 1) }, (_, i) => ({
      key: `p${from + i}`,
      label: String(from + i),
      page: from + i,
    }))
  const ellipsis = (side: string): PageItem => ({
    key: `ellipsis-${side}`,
    label: '…',
    page: null,
  })
  if (count <= 7) return range(1, last)
  if (current <= 3) {
    return [...range(1, Math.max(current + 2, 3)), ellipsis('r'), range(last, last)[0]]
  }
  if (current >= last - 2) {
    return [range(1, 1)[0], ellipsis('l'), ...range(Math.min(current - 2, last - 2), last)]
  }
  return [
    range(1, 1)[0],
    ellipsis('l'),
    ...range(current - 1, current + 1),
    ellipsis('r'),
    range(last, last)[0],
  ]
})
</script>

<template>
  <div
    v-if="total > 0"
    class="flex flex-col gap-2.5 border-t border-border px-3 py-2.5 sm:flex-row sm:items-center sm:justify-between sm:gap-3 sm:px-3.5"
  >
    <p class="text-[11px] text-muted-foreground shrink-0">{{ rangeLabel }}</p>
    <div class="flex w-full min-w-0 items-center justify-between gap-2 sm:w-auto sm:justify-end sm:gap-3">
      <label class="text-[11px] text-muted-foreground flex shrink-0 items-center gap-1.5 font-medium">
        <span class="hidden sm:inline">Per page</span>
        <span class="sm:hidden">Rows</span>
        <select
          v-model.number="pageSize"
          class="h-8 w-auto min-w-15 rounded-lg border border-border/60 bg-background px-2 py-1 text-xs outline-hidden focus:border-primary/50"
        >
          <option v-for="n in PAGE_SIZE_OPTIONS" :key="n" :value="n">{{ n }}</option>
        </select>
      </label>
      <div class="flex shrink-0 items-center gap-0.5 sm:gap-1">
        <button
          class="inline-flex items-center justify-center rounded-md px-2 sm:px-2.5 py-1 text-[11px] font-medium transition-colors"
          :class="page <= 1 ? 'text-muted-foreground/40 cursor-default' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
          :disabled="page <= 1"
          @click="page = page - 1"
        >
          Prev
        </button>
        <div class="hidden items-center gap-0.5 sm:flex">
          <template v-for="item in pageItems" :key="item.key">
            <span v-if="item.page === null" class="px-0.5 text-[11px] text-muted-foreground/50">
              {{ item.label }}
            </span>
            <button
              v-else
              :aria-current="item.page === page ? 'page' : undefined"
              :aria-label="`Page ${item.page}`"
              class="inline-flex items-center justify-center rounded-md px-2 py-1 text-[11px] font-medium tabular-nums transition-colors"
              :class="item.page === page ? 'bg-muted text-foreground' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
              @click="page = item.page"
            >
              {{ item.label }}
            </button>
          </template>
        </div>
        <span class="text-[11px] min-w-13 px-1 text-center tabular-nums text-muted-foreground sm:min-w-18">
          {{ page }} / {{ pageCount }}
        </span>
        <button
          class="inline-flex items-center justify-center rounded-md px-2 sm:px-2.5 py-1 text-[11px] font-medium transition-colors"
          :class="page >= pageCount ? 'text-muted-foreground/40 cursor-default' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
          :disabled="page >= pageCount"
          @click="page = page + 1"
        >
          Next
        </button>
      </div>
    </div>
  </div>
</template>
