<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, nextTick } from 'vue'
import { Search, X, ChevronDown } from '@lucide/vue'
import { cn } from '@/lib/utils'

const props = defineProps<{
  modelValue: string[]
  options: { id: string; label: string }[]
  placeholder?: string
}>()

const emit = defineEmits<{
  'update:modelValue': [value: string[]]
}>()

const open = ref(false)
const search = ref('')
const triggerRef = ref<HTMLElement | null>(null)
const dropdownRef = ref<HTMLElement | null>(null)
const inputRef = ref<HTMLInputElement | null>(null)
const dropdownStyle = ref<Record<string, string>>({})

const filteredOptions = computed(() => {
  const q = search.value.toLowerCase().trim()
  if (!q) return props.options.filter(o => !props.modelValue.includes(o.id))
  return props.options.filter(
    o => !props.modelValue.includes(o.id) && o.label.toLowerCase().includes(q)
  )
})

const selectedItems = computed(() =>
  props.modelValue.map(id => props.options.find(o => o.id === id)).filter(Boolean) as { id: string; label: string }[]
)

function toggle(id: string) {
  const current = props.modelValue.includes(id)
    ? props.modelValue.filter(v => v !== id)
    : [...props.modelValue, id]
  emit('update:modelValue', current)
}

function remove(id: string) {
  emit('update:modelValue', props.modelValue.filter(v => v !== id))
}

function reposition() {
  if (!triggerRef.value) return
  const rect = triggerRef.value.getBoundingClientRect()
  dropdownStyle.value = {
    position: 'fixed',
    top: `${rect.bottom + 4}px`,
    left: `${rect.left}px`,
    width: `${rect.width}px`,
  }
}

function toggleOpen(e: MouseEvent) {
  e.stopPropagation()
  open.value = !open.value
  if (open.value) {
    reposition()
    nextTick(() => inputRef.value?.focus())
  } else {
    search.value = ''
  }
}

function onBodyClick(e: MouseEvent) {
  if (!open.value) return
  const target = e.target as Node
  if (
    !triggerRef.value?.contains(target) &&
    !dropdownRef.value?.contains(target)
  ) {
    open.value = false
    search.value = ''
  }
}

onMounted(() => document.addEventListener('click', onBodyClick))
onUnmounted(() => document.removeEventListener('click', onBodyClick))
</script>

<template>
  <div ref="triggerRef" class="relative">
    <div
      :class="cn(
        'w-full min-h-[44px] rounded-xl border bg-input px-3 py-2 cursor-pointer transition-colors',
        'border-border',
        'flex flex-wrap items-center gap-1.5'
      )"
      @click="toggleOpen"
    >
      <span
        v-for="item in selectedItems"
        :key="item.id"
        class="inline-flex items-center gap-1 rounded-md bg-primary/10 px-2 py-0.5 text-xs font-medium text-primary"
      >
        {{ item.label }}
        <button type="button" class="hover:text-destructive transition-colors" @click.stop="remove(item.id)">
          <X class="h-3 w-3" />
        </button>
      </span>

      <span v-if="selectedItems.length === 0" class="text-xs text-muted-foreground">
        {{ placeholder || 'Select...' }}
      </span>

      <ChevronDown :class="cn('h-3.5 w-3.5 text-muted-foreground ml-auto transition-transform', open && 'rotate-180')" />
    </div>

    <Teleport to="body">
      <div
        v-if="open"
        ref="dropdownRef"
        :style="dropdownStyle"
        class="z-9999 rounded-xl border border-border bg-popover shadow-lg"
      >
        <div class="flex items-center gap-2 border-b border-border px-3 py-2">
          <Search class="h-3.5 w-3.5 text-muted-foreground shrink-0" />
          <input
            ref="inputRef"
            v-model="search"
            type="text"
            placeholder="Search..."
            class="flex-1 bg-transparent text-xs outline-hidden placeholder:text-muted-foreground"
            @click.stop
          />
        </div>

        <div class="max-h-48 overflow-y-auto py-1">
          <button
            v-for="opt in filteredOptions"
            :key="opt.id"
            type="button"
            class="w-full text-left px-3 py-1.5 text-xs hover:bg-muted transition-colors"
            @click.stop="toggle(opt.id)"
          >
            {{ opt.label }}
          </button>
          <div v-if="filteredOptions.length === 0" class="px-3 py-3 text-xs text-muted-foreground text-center">
            No matching options
          </div>
        </div>

        <div class="border-t border-border px-3 py-1.5 text-[10px] text-muted-foreground">
          {{ selectedItems.length }} selected
        </div>
      </div>
    </Teleport>
  </div>
</template>
