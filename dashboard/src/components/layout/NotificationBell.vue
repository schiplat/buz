<script setup lang="ts">
import { Bell, Check, X } from '@lucide/vue'
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import UiButton from '@/components/ui/UiButton.vue'
import { sandboxHostsApi, type PendingChange } from '@/api/sandboxHosts'
import { useAuthStore } from '@/stores/auth'
import { formatLocalDateTime as formatDateTime } from '@/utils/time'

/** Pending-approval notifications for ops users: the bell shows the count of
 *  undecided grant/host-config changes; the dropdown lists them and deep-links
 *  to the approvals inbox. Polls every 60s while mounted. */
const router = useRouter()
const auth = useAuthStore()

const open = ref(false)
const loading = ref(false)
const items = ref<PendingChange[]>([])
const rootEl = ref<HTMLElement | null>(null)
let poll: ReturnType<typeof setInterval> | null = null

/** Ops users own approvals; others have nothing to act on. */
const isOps = computed(() => auth.isOps)

async function refresh() {
  if (!isOps.value) return
  loading.value = true
  try {
    const res = await sandboxHostsApi.changes('pending')
    items.value = res.changes
  } catch (e) {
    console.error(e)
  } finally {
    loading.value = false
  }
}

function label(c: PendingChange): string {
  if (c.host_id === 'resource-grants') {
    const doc = c.after_document as unknown as Record<string, unknown>
    const verb = c.instance_id === 'grant' ? 'Grant' : 'Revoke'
    return `${verb} ${String(doc?.profile ?? '?')} ${c.instance_id === 'grant' ? '→' : 'from'} ${String(doc?.team_id ?? '?')}`
  }
  return `Host config ${c.host_id}/${c.instance_id}`
}

function openInbox() {
  open.value = false
  router.push({ name: 'sandbox-hosts', query: { approvals: '1' } })
}

function onDocClick(e: MouseEvent) {
  if (rootEl.value && !rootEl.value.contains(e.target as Node)) open.value = false
}
function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') open.value = false
}

onMounted(() => {
  if (!isOps.value) return
  void refresh()
  poll = setInterval(refresh, 60_000)
  document.addEventListener('click', onDocClick)
  document.addEventListener('keydown', onKeydown)
})
onUnmounted(() => {
  if (poll) clearInterval(poll)
  document.removeEventListener('click', onDocClick)
  document.removeEventListener('keydown', onKeydown)
})
</script>

<template>
  <div v-if="isOps" ref="rootEl" class="relative">
    <button
      type="button"
      class="relative inline-flex h-8 w-8 items-center justify-center rounded-full text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
      :aria-label="items.length ? `${items.length} pending approvals` : 'Notifications'"
      :aria-expanded="open"
      aria-haspopup="menu"
      @click.stop="open = !open"
    >
      <Bell class="h-4 w-4" />
      <span
        v-if="items.length"
        class="absolute -right-0.5 -top-0.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-red-500 px-1 text-[9.5px] font-semibold leading-none text-white"
      >{{ items.length > 9 ? '9+' : items.length }}</span>
    </button>

    <div
      v-if="open"
      role="menu"
      class="absolute right-0 top-full z-50 mt-2 w-80 overflow-hidden rounded-xl border border-border bg-background shadow-lg"
    >
      <div class="flex items-center justify-between border-b border-border/60 px-3.5 py-2.5">
        <p class="text-xs font-semibold uppercase tracking-wide text-muted-foreground">Pending approvals</p>
        <span v-if="items.length" class="rounded-full bg-amber-500/15 px-1.5 text-[10px] font-semibold text-amber-600">{{ items.length }}</span>
      </div>

      <div class="max-h-72 overflow-y-auto py-1">
        <p v-if="!items.length" class="px-3.5 py-6 text-center text-xs text-muted-foreground">Nothing pending — all caught up</p>
        <button
          v-for="c in items"
          :key="c.id"
          type="button"
          role="menuitem"
          class="flex w-full items-start gap-2.5 px-3.5 py-2 text-left hover:bg-muted"
          @click="openInbox"
        >
          <span
            class="mt-0.5 flex h-5 w-5 shrink-0 items-center justify-center rounded"
            :class="c.host_id === 'resource-grants' && c.instance_id === 'grant' ? 'bg-emerald-500/12 text-emerald-600' : c.host_id === 'resource-grants' ? 'bg-red-500/12 text-red-600' : 'bg-muted text-muted-foreground'"
          >
            <Check v-if="c.host_id === 'resource-grants' && c.instance_id === 'grant'" class="h-3 w-3" />
            <X v-else-if="c.host_id === 'resource-grants'" class="h-3 w-3" />
          </span>
          <span class="min-w-0 flex-1">
            <span class="block truncate text-xs font-medium text-foreground">{{ label(c) }}</span>
            <span class="block truncate text-[10.5px] text-muted-foreground">by {{ c.requested_by }} · {{ formatDateTime(c.created_at) }}</span>
          </span>
        </button>
      </div>

      <div class="border-t border-border/60 px-3.5 py-2">
        <UiButton size="sm" variant="ghost" class="w-full justify-center" @click="openInbox">
          Open approvals inbox
        </UiButton>
      </div>
    </div>
  </div>
</template>
