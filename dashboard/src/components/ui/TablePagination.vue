<script setup lang="ts">
import { PAGE_SIZE_OPTIONS } from '@/composables/useClientPagination'

const page = defineModel<number>('page', { required: true })
const pageSize = defineModel<number>('pageSize', { required: true })

defineProps<{
  total: number
  pageCount: number
  rangeLabel: string
}>()
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
