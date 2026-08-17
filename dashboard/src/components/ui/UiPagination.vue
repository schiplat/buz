<script setup lang="ts">
import { computed } from 'vue'

const props = withDefaults(defineProps<{
  page: number
  total: number
  pageSize?: number
}>(), {
  pageSize: 15,
})

const emit = defineEmits<{
  (e: 'update:page', v: number): void
}>()

const totalPages = computed(() => Math.max(1, Math.ceil(props.total / props.pageSize)))
const start = computed(() => (props.page - 1) * props.pageSize + 1)
const end = computed(() => Math.min(props.page * props.pageSize, props.total))

function go(p: number) {
  const v = Math.max(1, Math.min(p, totalPages.value))
  if (v !== props.page) emit('update:page', v)
}
</script>

<template>
  <div v-if="totalPages > 1 || total > pageSize" class="flex items-center justify-between px-4 py-2.5 border-t border-border/20 bg-muted/5">
    <span class="text-[11px] text-muted-foreground">{{ start }}–{{ end }} of {{ total }}</span>
    <div class="flex items-center gap-1">
      <button
        class="px-2.5 py-1 rounded text-[11px] transition-colors"
        :class="page === 1 ? 'text-muted-foreground/40 cursor-default' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
        :disabled="page === 1"
        @click="go(page - 1)"
      >Prev</button>
      <button
        v-for="p in totalPages"
        :key="p"
        class="w-7 h-7 rounded text-[11px] tabular-nums transition-colors"
        :class="p === page ? 'bg-primary text-primary-foreground font-semibold' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
        @click="go(p)"
      >{{ p }}</button>
      <button
        class="px-2.5 py-1 rounded text-[11px] transition-colors"
        :class="page === totalPages ? 'text-muted-foreground/40 cursor-default' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
        :disabled="page === totalPages"
        @click="go(page + 1)"
      >Next</button>
    </div>
  </div>
</template>
