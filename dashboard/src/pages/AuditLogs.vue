<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useRouter } from 'vue-router'
import { ScrollText, X } from '@lucide/vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import TablePagination from '@/components/ui/TablePagination.vue'
import { auditApi, type AuditLogItem } from '@/api/audit'
import { formatAuditJson, toSqliteDateBound } from '@/utils/auditDisplay'
import { useAuthStore } from '@/stores/auth'

const auth = useAuthStore()
const router = useRouter()

const loading = ref(true)
const items = ref<AuditLogItem[]>([])
const total = ref(0)
const page = ref(1)
const pageSize = ref(20)

const filterAction = ref('')
const filterActor = ref('')
const filterResourceType = ref('')
const searchQuery = ref('')
const filterFrom = ref('')
const filterTo = ref('')

const drawerOpen = ref(false)
const active = ref<AuditLogItem | null>(null)

const pageCount = computed(() => Math.max(1, Math.ceil(total.value / pageSize.value) || 1))
const rangeLabel = computed(() => {
  if (total.value === 0) return '0 of 0'
  const start = (page.value - 1) * pageSize.value + 1
  const end = Math.min(page.value * pageSize.value, total.value)
  return `${start}–${end} of ${total.value}`
})

const ACTION_OPTIONS = [
  '',
  'auth.login',
  'secret.create',
  'secret.update',
  'secret.delete',
  'variable.create',
  'variable.update',
  'variable.delete',
  'host.create',
  'host.update',
  'host.delete',
  'user.create',
  'user.update',
  'user.delete',
  'team.create',
  'team.update',
  'team.delete',
  'team.member_upsert',
  'team.member_remove',
  'pipeline.create',
  'pipeline.update',
  'pipeline.delete',
  'pipeline.toggle_active',
  'pipeline.trigger',
  'git_hook.create',
  'git_hook.delete',
  'api_token.create',
  'api_token.revoke',
]

const RESOURCE_TYPE_OPTIONS = [
  '',
  'user',
  'team',
  'secret',
  'variable',
  'host',
  'pipeline',
  'git_hook',
  'api_token',
]

function openDetail(row: AuditLogItem) {
  active.value = row
  drawerOpen.value = true
}

function closeDetail() {
  drawerOpen.value = false
  setTimeout(() => {
    if (!drawerOpen.value) active.value = null
  }, 300)
}

async function load() {
  if (!auth.isPlatformAdmin) {
    router.replace('/dashboard')
    return
  }
  loading.value = true
  try {
    const res = await auditApi.list({
      page: page.value,
      page_size: pageSize.value,
      action: filterAction.value || undefined,
      actor: filterActor.value.trim() || undefined,
      resource_type: filterResourceType.value || undefined,
      q: searchQuery.value.trim() || undefined,
      from: toSqliteDateBound(filterFrom.value),
      to: toSqliteDateBound(filterTo.value),
    })
    items.value = res.items ?? []
    total.value = res.total ?? 0
    if (page.value > pageCount.value) page.value = pageCount.value
  } catch {
    items.value = []
    total.value = 0
  } finally {
    loading.value = false
  }
}

watch([page, pageSize], () => {
  void load()
})

watch(
  [filterAction, filterActor, filterResourceType, searchQuery, filterFrom, filterTo],
  () => {
    if (page.value !== 1) {
      page.value = 1
      return
    }
    void load()
  },
)

function formatTime(raw?: string | null): string {
  if (!raw) return '—'
  return raw.slice(0, 19).replace('T', ' ')
}

/** Split `secret.create` → { ns: 'secret', op: 'create' } for quieter table cells. */
function splitAction(action: string): { ns: string; op: string } {
  const i = action.lastIndexOf('.')
  if (i <= 0) return { ns: '', op: action }
  return { ns: action.slice(0, i), op: action.slice(i + 1) }
}

onMounted(load)
</script>

<template>
  <div class="page-stack">
    <PageHeader
      title="Audit logs"
      :description="auth.isSuperAdmin ? 'Super-admin view of all operation audit records. Sensitive fields are redacted.' : 'Platform-admin view of your own operation audit records. Sensitive fields are redacted.'"
    />

    <div v-if="loading && items.length === 0" class="surface-card p-8 text-center type-meta">
      Loading…
    </div>

    <div v-else class="surface-card overflow-hidden">
      <div class="px-5 py-3 border-b border-border/30 flex items-center gap-2.5 flex-wrap">
        <input
          v-model="searchQuery"
          type="text"
          placeholder="Search resource, action, actor…"
          class="w-full max-w-56 rounded-lg border border-border/60 bg-background px-3 py-1.5 text-[13px] outline-hidden transition-colors focus:border-primary/50 focus:ring-1 focus:ring-primary/10"
        />
        <select
          v-model="filterAction"
          class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-[13px] outline-hidden transition-colors focus:border-primary/50"
        >
          <option value="">All actions</option>
          <option v-for="a in ACTION_OPTIONS.filter(Boolean)" :key="a" :value="a">{{ a }}</option>
        </select>
        <select
          v-model="filterResourceType"
          class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-[13px] outline-hidden transition-colors focus:border-primary/50"
        >
          <option value="">All resources</option>
          <option v-for="r in RESOURCE_TYPE_OPTIONS.filter(Boolean)" :key="r" :value="r">{{ r }}</option>
        </select>
        <input
          v-model="filterActor"
          type="text"
          placeholder="Actor…"
          class="w-36 rounded-lg border border-border/60 bg-background px-3 py-1.5 text-[13px] outline-hidden transition-colors focus:border-primary/50"
        />
        <input
          v-model="filterFrom"
          type="datetime-local"
          class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-[13px] outline-hidden transition-colors focus:border-primary/50"
          title="From"
        />
        <input
          v-model="filterTo"
          type="datetime-local"
          class="rounded-lg border border-border/60 bg-background px-3 py-1.5 text-[13px] outline-hidden transition-colors focus:border-primary/50"
          title="To"
        />
      </div>

      <div v-if="items.length === 0" class="flex flex-col items-center justify-center py-16 text-muted-foreground">
        <ScrollText class="h-10 w-10 mb-3 text-muted-foreground/20" />
        <p class="text-sm">No audit logs</p>
      </div>

      <template v-else>
        <div class="data-table-scroll">
          <table class="data-table min-w-[720px]">
            <thead>
              <tr>
                <th class="w-44">Time</th>
                <th class="w-32">Actor</th>
                <th class="w-40">Action</th>
                <th>Resource</th>
                <th class="w-24">Team</th>
                <th class="w-18 text-right" />
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="row in items"
                :key="row.id"
                class="clickable"
                @click="openDetail(row)"
              >
                <td class="text-xs text-muted-foreground tabular-nums whitespace-nowrap">
                  {{ formatTime(row.created_at) }}
                </td>
                <td class="whitespace-nowrap">
                  <span class="font-mono text-[12px] font-medium leading-snug text-foreground/90">{{ row.actor_username || '—' }}</span>
                </td>
                <td>
                  <div
                    class="text-[13px] leading-snug"
                    :title="row.action"
                  >
                    <template v-for="a in [splitAction(row.action)]" :key="`${row.id}-action`">
                      <span v-if="a.ns" class="text-muted-foreground">{{ a.ns }}.</span><span class="font-medium">{{ a.op }}</span>
                    </template>
                  </div>
                </td>
                <td>
                  <div
                    class="font-mono text-[12px] font-medium leading-snug text-foreground/90 truncate max-w-[18rem]"
                    :title="row.resource_name || row.resource_id || undefined"
                  >
                    {{ row.resource_name || row.resource_id || '—' }}
                  </div>
                  <div v-if="row.resource_type" class="type-meta mt-0.5">
                    {{ row.resource_type }}
                  </div>
                </td>
                <td class="text-xs text-muted-foreground truncate" :title="row.team_id || undefined">
                  {{ row.team_id ? row.team_id.slice(0, 8) : '—' }}
                </td>
                <td class="text-right" @click.stop>
                  <UiButton variant="ghost" size="sm" @click="openDetail(row)">View</UiButton>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <TablePagination
          v-model:page="page"
          v-model:pageSize="pageSize"
          :total="total"
          :page-count="pageCount"
          :range-label="rangeLabel"
        />
      </template>
    </div>

    <Teleport to="body">
      <div
        v-show="drawerOpen"
        class="fixed inset-0 z-50 flex justify-end"
        :class="drawerOpen ? 'pointer-events-auto' : 'pointer-events-none'"
      >
        <div
          class="absolute inset-0 transition-all duration-300"
          :class="drawerOpen ? 'bg-black/40' : 'bg-transparent'"
          @click="closeDetail"
        />
        <div
          class="scrollbar-sidebar relative z-10 w-full max-w-lg h-full bg-background border-l border-border shadow-2xl overflow-y-auto transition-transform duration-300"
          :class="drawerOpen ? 'translate-x-0' : 'translate-x-full'"
        >
          <div class="sticky top-0 z-10 flex items-center justify-between border-b border-border bg-background px-6 py-4">
            <div class="min-w-0">
              <h2 class="text-base font-semibold tracking-tight truncate">
                {{ active?.action || 'Audit entry' }}
              </h2>
              <p class="type-meta mt-0.5 truncate">
                {{ active?.actor_username || '—' }} · {{ formatTime(active?.created_at) }}
              </p>
            </div>
            <button
              type="button"
              class="inline-flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground transition-colors shrink-0"
              @click="closeDetail"
            >
              <X class="h-4 w-4" />
            </button>
          </div>

          <div v-if="active" class="px-6 py-5 space-y-5">
            <div class="grid grid-cols-2 gap-4">
              <div>
                <div class="type-label mb-1">Resource</div>
                <div class="text-sm font-medium">{{ active.resource_name || '—' }}</div>
                <div class="type-meta mt-0.5 break-all">
                  {{ active.resource_type || '—' }}<template v-if="active.resource_id"> · {{ active.resource_id }}</template>
                </div>
              </div>
              <div>
                <div class="type-label mb-1">Team</div>
                <div class="text-sm break-all">{{ active.team_id || '—' }}</div>
              </div>
            </div>

            <div>
              <div class="type-label mb-2">Before</div>
              <pre class="rounded-lg border border-border/60 bg-muted/30 p-3 text-xs font-mono leading-relaxed overflow-x-auto whitespace-pre-wrap break-all">{{ formatAuditJson(active.before) }}</pre>
            </div>
            <div>
              <div class="type-label mb-2">After</div>
              <pre class="rounded-lg border border-border/60 bg-muted/30 p-3 text-xs font-mono leading-relaxed overflow-x-auto whitespace-pre-wrap break-all">{{ formatAuditJson(active.after) }}</pre>
            </div>
            <div v-if="active.meta != null">
              <div class="type-label mb-2">Meta</div>
              <pre class="rounded-lg border border-border/60 bg-muted/30 p-3 text-xs font-mono leading-relaxed overflow-x-auto whitespace-pre-wrap break-all">{{ formatAuditJson(active.meta) }}</pre>
            </div>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>
