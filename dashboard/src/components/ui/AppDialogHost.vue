<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue'
import UiButton from '@/components/ui/UiButton.vue'
import { useAppDialogState, type AppDialogState } from '@/composables/useAppDialog'

const dialog = useAppDialogState()
const inputEl = ref<HTMLInputElement | HTMLTextAreaElement | null>(null)
/** Local draft — avoids nested reactive mutation issues on the shared dialog state. */
const promptDraft = ref('')
/** Checkbox draft for confirm dialogs that carry a checkboxLabel. */
const checkboxDraft = ref(false)

const hasCheckbox = computed(
  () => dialog.value?.kind === 'confirm' && dialog.value.checkboxLabel !== null,
)

const promptOkDisabled = computed(() => {
  if (dialog.value?.kind !== 'prompt') return false
  if (!dialog.value.required) return false
  return !promptDraft.value.trim()
})

watch(
  dialog,
  async (d: AppDialogState) => {
    if (d?.kind === 'prompt') {
      promptDraft.value = d.value
      await nextTick()
      inputEl.value?.focus()
    }
    if (d?.kind === 'confirm' && d.checkboxLabel !== null) {
      checkboxDraft.value = d.checkboxDefault
    }
  },
  { flush: 'post' },
)

function finish(run: () => void) {
  // Defer close so the click that submitted cannot fall through to UI below.
  nextTick(() => run())
}

function onBackdrop() {
  const d = dialog.value
  if (!d) return
  if (d.kind === 'alert') finish(() => d.resolve())
  else if (d.kind === 'confirm') finish(() => d.resolve(false, checkboxDraft.value))
  else finish(() => d.resolve(null))
}

function onPrimary() {
  const d = dialog.value
  if (!d) return
  if (d.kind === 'alert') finish(() => d.resolve())
  else if (d.kind === 'confirm') finish(() => d.resolve(true, checkboxDraft.value))
  else {
    if (d.required && !promptDraft.value.trim()) return
    const value = promptDraft.value
    finish(() => d.resolve(value))
  }
}

function onCancel() {
  const d = dialog.value
  if (!d) return
  if (d.kind === 'confirm') finish(() => d.resolve(false, checkboxDraft.value))
  else if (d.kind === 'prompt') finish(() => d.resolve(null))
}
</script>

<template>
  <Teleport to="body">
    <div
      v-if="dialog"
      class="fixed inset-0 z-[100] flex items-center justify-center"
      role="dialog"
      aria-modal="true"
    >
      <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="onBackdrop" />
      <div
        class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6"
        @click.stop
      >
        <h3 class="text-[15px] font-semibold mb-2">{{ dialog.title }}</h3>
        <p class="text-sm text-muted-foreground whitespace-pre-wrap">{{ dialog.message }}</p>

        <div v-if="dialog.kind === 'prompt'" class="mt-4">
          <label class="block text-[12px] font-medium mb-1">
            {{ dialog.label }}
            <span v-if="dialog.required" class="text-red-500">*</span>
          </label>
          <textarea
            v-if="dialog.multiline"
            ref="inputEl"
            v-model="promptDraft"
            rows="4"
            class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 resize-y"
            :placeholder="dialog.placeholder"
            @keydown.meta.enter.prevent="onPrimary"
          />
          <input
            v-else
            ref="inputEl"
            v-model="promptDraft"
            type="text"
            class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50"
            :placeholder="dialog.placeholder"
            @keydown.enter.prevent="onPrimary"
          />
        </div>

        <label
          v-if="hasCheckbox && dialog.kind === 'confirm'"
          class="mt-4 flex items-center gap-2 text-sm text-muted-foreground cursor-pointer select-none"
        >
          <input
            v-model="checkboxDraft"
            type="checkbox"
            class="h-4 w-4 rounded border-border/60 accent-primary cursor-pointer"
          />
          {{ dialog.checkboxLabel }}
        </label>

        <div class="flex justify-end gap-3 mt-6 pt-5 border-t border-border/30">
          <UiButton
            v-if="dialog.kind !== 'alert'"
            variant="ghost"
            size="sm"
            @click="onCancel"
          >
            {{ dialog.kind === 'confirm' || dialog.kind === 'prompt' ? dialog.cancelLabel : 'Cancel' }}
          </UiButton>
          <UiButton
            size="sm"
            :class="dialog.kind === 'confirm' && dialog.danger ? 'bg-red-600 hover:bg-red-600/90 text-white' : ''"
            :disabled="promptOkDisabled"
            @click="onPrimary"
          >
            {{ dialog.confirmLabel }}
          </UiButton>
        </div>
      </div>
    </div>
  </Teleport>
</template>
