<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import { Plus, X, Copy, Check, Cpu, HardDrive, Monitor, Snowflake, Trash2, ScrollText } from '@lucide/vue'
import { runnersApi, type Runner } from '@/api/runners'
import UiPagination from '@/components/ui/UiPagination.vue'

const runners = ref<Runner[]>([])
const loading = ref(true)

const runnerPage = ref(1)
const pageSize = 15

const pagedRunners = computed(() => {
  const start = (runnerPage.value - 1) * pageSize
  return runners.value.slice(start, start + pageSize)
})

// Create dialog
const showCreate = ref(false)
const newName = ref('')
const newLabels = ref('linux-x64')
const creating = ref(false)
const createdToken = ref<string | null>(null)
const copied = ref(false)

// Drawer
const activeRunner = ref<Runner | null>(null)
const drawerOpen = ref(false)
const detailTab = ref<'overview' | 'network' | 'logs'>('overview')

onMounted(async () => { await loadRunners() })

async function loadRunners() {
  loading.value = true
  try { runners.value = await runnersApi.list(); runnerPage.value = 1 } catch (e) { console.error(e) }
  finally { loading.value = false }
}

function openDetail(r: Runner) { activeRunner.value = r; detailTab.value = 'overview'; drawerOpen.value = true; loadRunnerEvents(r.id) }
function closeDetail() { drawerOpen.value = false; setTimeout(() => { if (!drawerOpen.value) activeRunner.value = null }, 300) }

// ── Runner events log ──
interface RunnerEvent { id: string; event_type: string; message: string; created_at: string }
const runnerEvents = ref<RunnerEvent[]>([])
const eventsLoading = ref(false)
const eventsPage = ref(1)
const eventsPageSize = 15
const eventsTotal = ref(0)
const eventsTotalPages = ref(0)

async function loadRunnerEvents(runnerId: string) {
  eventsLoading.value = true
  try {
    const result = await runnersApi.events(runnerId, eventsPage.value, eventsPageSize)
    runnerEvents.value = result.items
    eventsTotal.value = result.total
    eventsTotalPages.value = result.total_pages
  } catch {
    runnerEvents.value = []
    eventsTotal.value = 0
    eventsTotalPages.value = 0
  } finally {
    eventsLoading.value = false
  }
}

function eventsGo(page: number) {
  eventsPage.value = Math.max(1, Math.min(page, eventsTotalPages.value))
  if (activeRunner.value) loadRunnerEvents(activeRunner.value.id)
}

function openCreate() {
  newName.value = ''; newLabels.value = 'linux-x64'; createdToken.value = null; copied.value = false; showCreate.value = true
}

async function handleCreate() {
  if (!newName.value.trim()) return
  creating.value = true
  try {
    const labels = newLabels.value.split(',').map(s => s.trim()).filter(Boolean)
    const resp = await runnersApi.create(newName.value.trim(), labels)
    createdToken.value = resp.register_token
    await loadRunners()
  } catch (e) { console.error(e); alert('Failed to create runner') }
  finally { creating.value = false }
}

async function copyToken() {
  if (!createdToken.value) return
  try { await navigator.clipboard.writeText(createdToken.value); copied.value = true; setTimeout(() => (copied.value = false), 2000) }
  catch { /* fallback */ }
}

async function handleToggleFreeze(r: Runner) {
  try {
    const result = await runnersApi.toggleFreeze(r.id)
    r.is_frozen = result.is_frozen
  } catch (e) { console.error(e) }
}

async function handleDeleteRunner(r: Runner) {
  if (!confirm(`Delete runner "${r.name}"? This cannot be undone.`)) return
  try {
    await runnersApi.delete(r.id)
    await loadRunners()
  } catch (e) { console.error(e); alert('Failed to delete runner') }
}

// Formatters
const fmtMB = (mb: number) => mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${mb} MB`
const fmtGB = (gb: number) => `${gb.toFixed(1)} GB`
const fmtUptime = (s: number) => {
  const d = Math.floor(s / 86400), h = Math.floor((s % 86400) / 3600), m = Math.floor((s % 3600) / 60)
  return d > 0 ? `${d}d ${h}h` : h > 0 ? `${h}h ${m}m` : `${m}m`
}
const memPct = (t: number, u: number) => t === 0 ? 0 : (u / t) * 100
const diskPct = (t: number, u: number) => t === 0 ? 0 : (u / t) * 100
const fmtPct = (p: number) => `${p.toFixed(0)}%`
const hasInfo = (r: Runner | null) => !!(r?.system_info?.hostname)

const eventBadge = (t: string) => {
  const m: Record<string, string> = {
    created: 'bg-slate-500/10 text-slate-600',
    registered: 'bg-emerald-500/10 text-emerald-600',
    job_dispatched: 'bg-blue-500/10 text-blue-600',
    frozen: 'bg-sky-500/10 text-sky-600',
    unfrozen: 'bg-sky-500/10 text-sky-600',
    offline: 'bg-red-500/10 text-red-500',
  }
  return m[t] || 'bg-muted text-muted-foreground'
}
</script>

<template>
  <div class="space-y-6">
    <PageHeader title="Runners" description="Registered runners and their current status.">
      <template #actions>
        <UiButton size="sm" @click="openCreate"><Plus class="h-4 w-4" /> New Runner</UiButton>
      </template>
    </PageHeader>

    <div class="surface-card overflow-hidden">
      <div v-if="loading" class="px-4 py-10 text-center type-meta">Loading...</div>

      <template v-else-if="runners.length > 0">
        <div class="data-table-scroll">
          <table class="data-table">
            <thead>
              <tr>
                <th>Name</th><th>Hostname</th><th>IP</th><th>Labels</th>
                <th>Status</th><th>Version</th><th>Last Seen</th>
                <th class="text-right">Actions</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="r in pagedRunners" :key="r.id">
                <td class="font-medium text-primary cursor-pointer hover:underline" @click="openDetail(r)">{{ r.name }}
                  <span v-if="r.is_frozen" class="ml-1.5 text-[10px] text-sky-500 font-normal">(frozen)</span>
                </td>
                <td class="font-mono text-xs text-muted-foreground">{{ r.system_info?.hostname || '—' }}</td>
                <td class="font-mono text-xs text-muted-foreground">{{ r.system_info?.ip_addresses?.[0] || '—' }}</td>
                <td><span v-for="l in r.labels" :key="l" class="mono-chip mr-1">{{ l }}</span></td>
                <td><StatusBadge :status="r.status" /></td>
                <td class="font-mono text-xs text-muted-foreground">{{ r.version || '—' }}</td>
                <td class="text-xs text-muted-foreground">{{ r.last_seen || '—' }}</td>
                <td class="text-right">
                  <div class="flex items-center justify-end gap-1">
                    <UiButton variant="ghost" size="sm" @click="handleToggleFreeze(r)" :title="r.is_frozen ? 'Unfreeze' : 'Freeze'">
                      <Snowflake class="h-3.5 w-3.5" :class="r.is_frozen ? 'text-sky-500' : 'text-muted-foreground'" />
                    </UiButton>
                    <UiButton variant="ghost" size="sm" @click="handleDeleteRunner(r)" title="Delete">
                      <Trash2 class="h-3.5 w-3.5 text-destructive" />
                    </UiButton>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
          <UiPagination v-model:page="runnerPage" :total="runners.length" :page-size="pageSize" />
        </div>
      </template>
      <div v-else class="px-4 py-10 text-center type-meta">No runners registered yet</div>
    </div>

    <!-- ── Right-side detail drawer ── -->
    <Teleport to="body">
      <div
        v-show="drawerOpen"
        class="fixed inset-0 z-50 flex justify-end"
        :class="drawerOpen ? 'pointer-events-auto' : 'pointer-events-none'"
      >
        <div class="absolute inset-0 transition-all duration-300" :class="drawerOpen ? 'bg-black/40' : 'bg-transparent'" @click="closeDetail"></div>

        <div class="relative z-10 w-full max-w-lg h-full bg-background border-l border-border shadow-2xl overflow-y-auto transition-transform duration-300" :class="drawerOpen ? 'translate-x-0' : 'translate-x-full'">
          <!-- Header -->
          <div class="sticky top-0 z-10 flex items-center justify-between border-b border-border bg-background px-6 py-4">
            <div class="flex items-center gap-3 min-w-0">
              <h2 class="text-base font-semibold tracking-tight truncate">{{ activeRunner?.name || 'Runner' }}</h2>
              <StatusBadge v-if="activeRunner" :status="activeRunner.status" />
            </div>
            <button class="inline-flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground transition-colors shrink-0" @click="closeDetail">
              <X class="h-4 w-4" />
            </button>
          </div>

          <!-- Body -->
          <div class="px-6 py-5 space-y-5" v-if="activeRunner">
            <!-- Quick stats -->
            <div v-if="hasInfo(activeRunner)" class="flex flex-wrap items-center gap-x-5 gap-y-1 text-[11px] text-muted-foreground">
              <span>Runner v{{ activeRunner.system_info!.runner_version }}</span>
              <span>{{ activeRunner.system_info!.cpu_arch }} · {{ activeRunner.system_info!.cpu_cores }} cores</span>
              <span>Uptime {{ fmtUptime(activeRunner.system_info!.uptime_seconds) }}</span>
            </div>

            <!-- OS -->
            <div v-if="hasInfo(activeRunner)" class="rounded-xl border border-border/60 bg-muted/30 px-3.5 py-3">
              <div class="text-[11px] font-semibold text-muted-foreground uppercase tracking-wider mb-1.5">Operating System</div>
              <div class="text-sm">{{ activeRunner.system_info!.os }}</div>
              <div class="text-xs text-muted-foreground font-mono mt-1">Kernel {{ activeRunner.system_info!.kernel_version }}</div>
            </div>

            <!-- Labels -->
            <div>
              <div class="text-[11px] font-semibold text-muted-foreground uppercase tracking-wider mb-2">Labels</div>
              <div class="flex flex-wrap gap-1">
                <span v-for="l in activeRunner.labels" :key="l" class="mono-chip">{{ l }}</span>
                <span v-if="activeRunner.labels.length === 0" class="text-xs text-muted-foreground">—</span>
              </div>
            </div>

            <!-- Waiting -->
            <div v-if="!hasInfo(activeRunner)" class="rounded-xl border border-border/60 bg-muted/30 px-4 py-6 text-center">
              <Monitor class="h-8 w-8 mx-auto text-muted-foreground/40 mb-3" />
              <p class="text-sm text-muted-foreground">Waiting for first heartbeat</p>
              <p class="text-xs text-muted-foreground/60 mt-1">System info will appear once the Runner reports in.</p>
            </div>

            <!-- Tabs -->
            <template v-if="hasInfo(activeRunner)">
              <div class="flex rounded-xl bg-muted p-1">
                <button
                  class="flex-1 rounded-lg px-3 py-2 text-xs font-medium transition-colors"
                  :class="detailTab === 'overview' ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'"
                  @click="detailTab = 'overview'"
                >Overview</button>
                <button
                  class="flex-1 rounded-lg px-3 py-2 text-xs font-medium transition-colors"
                  :class="detailTab === 'network' ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'"
                  @click="detailTab = 'network'"
                >Network</button>
                <button
                  class="flex-1 rounded-lg px-3 py-2 text-xs font-medium transition-colors"
                  :class="detailTab === 'logs' ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'"
                  @click="detailTab = 'logs'"
                >Log</button>
              </div>

              <!-- Overview tab: CPU / Memory / Disk -->
              <div v-show="detailTab === 'overview'" class="grid grid-cols-1 gap-3">
                <!-- CPU -->
                <div class="rounded-xl border border-border/60 bg-background px-3.5 py-3">
                  <div class="flex items-center gap-2 mb-1.5">
                    <Cpu class="h-3.5 w-3.5 text-primary" />
                    <span class="text-[11px] font-semibold text-muted-foreground uppercase tracking-wider">CPU</span>
                  </div>
                  <div class="flex items-baseline gap-2">
                    <span
                      class="text-base font-bold tabular-nums"
                      :class="(activeRunner.system_info!.cpu_usage_percent ?? 0) > 80 ? 'text-amber-500' : ''"
                    >{{ (activeRunner.system_info!.cpu_usage_percent ?? 0).toFixed(1) }}%</span>
                    <span class="text-xs text-muted-foreground">{{ activeRunner.system_info!.cpu_cores }} cores</span>
                  </div>
                  <div class="flex items-center gap-2 mt-2">
                    <div class="h-1 flex-1 rounded-full bg-muted">
                      <div
                        class="h-1 rounded-full transition-all"
                        :class="(activeRunner.system_info!.cpu_usage_percent ?? 0) > 80 ? 'bg-amber-500' : 'bg-emerald-400'"
                        :style="{ width: (activeRunner.system_info!.cpu_usage_percent ?? 0) + '%' }"
                      ></div>
                    </div>
                  </div>
                  <div class="text-[10px] text-muted-foreground mt-1">{{ activeRunner.system_info!.cpu_model }}</div>
                </div>

                <!-- Memory -->
                <div class="rounded-xl border border-border/60 bg-background px-3.5 py-3">
                  <div class="flex items-center gap-2 mb-1.5">
                    <Monitor class="h-3.5 w-3.5 text-primary" />
                    <span class="text-[11px] font-semibold text-muted-foreground uppercase tracking-wider">Memory</span>
                  </div>
                  <div class="flex items-baseline gap-2">
                    <span class="text-base font-bold tabular-nums">{{ fmtPct(memPct(activeRunner.system_info!.memory_total_mb, activeRunner.system_info!.memory_used_mb)) }}</span>
                    <span class="text-xs text-muted-foreground">{{ fmtMB(activeRunner.system_info!.memory_used_mb) }} / {{ fmtMB(activeRunner.system_info!.memory_total_mb) }}</span>
                  </div>
                  <div class="flex items-center gap-2 mt-2">
                    <div class="h-1 flex-1 rounded-full bg-muted">
                      <div class="h-1 rounded-full transition-all" :class="memPct(activeRunner.system_info!.memory_total_mb, activeRunner.system_info!.memory_used_mb) > 80 ? 'bg-amber-500' : 'bg-emerald-400'" :style="{ width: memPct(activeRunner.system_info!.memory_total_mb, activeRunner.system_info!.memory_used_mb) + '%' }"></div>
                    </div>
                  </div>
                  <div v-if="activeRunner.system_info!.swap_total_mb > 0" class="text-[10px] text-muted-foreground mt-1.5">Swap {{ fmtMB(activeRunner.system_info!.swap_used_mb) }} / {{ fmtMB(activeRunner.system_info!.swap_total_mb) }}</div>
                </div>

                <!-- Disk -->
                <div class="rounded-xl border border-border/60 bg-background px-3.5 py-3">
                  <div class="flex items-center gap-2 mb-1.5">
                    <HardDrive class="h-3.5 w-3.5 text-primary" />
                    <span class="text-[11px] font-semibold text-muted-foreground uppercase tracking-wider">Disk</span>
                  </div>
                  <div class="flex items-baseline gap-2">
                    <span class="text-base font-bold tabular-nums">{{ fmtPct(diskPct(activeRunner.system_info!.disk_total_gb, activeRunner.system_info!.disk_used_gb)) }}</span>
                    <span class="text-xs text-muted-foreground">{{ fmtGB(activeRunner.system_info!.disk_used_gb) }} / {{ fmtGB(activeRunner.system_info!.disk_total_gb) }}</span>
                  </div>
                  <div class="flex items-center gap-2 mt-2">
                    <div class="h-1 flex-1 rounded-full bg-muted">
                      <div class="h-1 rounded-full transition-all" :class="diskPct(activeRunner.system_info!.disk_total_gb, activeRunner.system_info!.disk_used_gb) > 80 ? 'bg-amber-500' : 'bg-emerald-400'" :style="{ width: diskPct(activeRunner.system_info!.disk_total_gb, activeRunner.system_info!.disk_used_gb) + '%' }"></div>
                    </div>
                  </div>
                </div>
              </div>

              <!-- Network tab: IPs + interfaces detail -->
              <div v-show="detailTab === 'network'" class="space-y-3">
                <!-- IP addresses -->
                <div class="rounded-xl border border-border/60 bg-background px-3.5 py-3">
                  <div class="text-[11px] font-semibold text-muted-foreground uppercase tracking-wider mb-2">IP Addresses</div>
                  <div v-if="activeRunner.system_info!.ip_addresses && activeRunner.system_info!.ip_addresses.length > 0" class="space-y-1">
                    <div v-for="ip in activeRunner.system_info!.ip_addresses" :key="ip" class="flex items-center gap-2 text-xs font-mono">
                      <div class="h-1.5 w-1.5 rounded-full bg-emerald-400 shrink-0"></div>
                      <span>{{ ip }}</span>
                    </div>
                  </div>
                  <div v-else class="text-xs text-muted-foreground">No external IPs</div>
                </div>

                <!-- Interfaces -->
                <div class="rounded-xl border border-border/60 bg-background px-3.5 py-3">
                  <div class="text-[11px] font-semibold text-muted-foreground uppercase tracking-wider mb-2">
                    Interfaces
                    <span v-if="activeRunner.system_info!.network_interfaces && activeRunner.system_info!.network_interfaces.length > 0" class="text-muted-foreground/60 ml-1">
                      ({{ activeRunner.system_info!.network_interfaces.length }})
                    </span>
                  </div>
                  <div v-if="activeRunner.system_info!.network_interfaces && activeRunner.system_info!.network_interfaces.length > 0">
                    <div class="flex flex-wrap gap-1.5">
                      <span
                        v-for="nic in activeRunner.system_info!.network_interfaces"
                        :key="nic.name"
                        class="inline-flex items-center gap-1.5 rounded-lg border border-border/50 bg-muted/40 px-2.5 py-1.5 text-xs font-mono"
                      >
                        <span class="font-medium text-foreground">{{ nic.name }}</span>
                        <span v-if="nic.ips[0]" class="text-muted-foreground">{{ nic.ips[0] }}</span>
                        <span v-else class="text-[10px] text-muted-foreground/50">{{ nic.mac }}</span>
                      </span>
                    </div>
                  </div>
                  <div v-else class="text-xs text-muted-foreground">No interfaces reported</div>
                </div>
              </div>
            </template>

            <!-- Logs tab -->
            <template v-if="hasInfo(activeRunner)">
              <div v-show="detailTab === 'logs'" class="space-y-2">
                <div v-if="eventsLoading" class="text-center py-8 text-xs text-muted-foreground">Loading…</div>
                <div v-else-if="runnerEvents.length === 0" class="rounded-xl border border-border/60 bg-background px-4 py-8 text-center">
                  <ScrollText class="h-8 w-8 mx-auto text-muted-foreground/30 mb-3" />
                  <p class="text-sm text-muted-foreground">No events recorded yet</p>
                  <p class="text-xs text-muted-foreground/60 mt-1">Events appear when the runner registers, claims jobs, or changes state.</p>
                </div>
                <div v-else class="space-y-1.5">
                  <div
                    v-for="ev in runnerEvents"
                    :key="ev.id"
                    class="rounded-lg border border-border/40 bg-background px-3.5 py-2.5 flex items-start gap-3"
                  >
                    <span
                      class="inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide mt-0.5 shrink-0"
                      :class="eventBadge(ev.event_type)"
                    >{{ ev.event_type }}</span>
                    <div class="min-w-0 flex-1">
                      <p class="text-xs text-foreground leading-snug">{{ ev.message }}</p>
                      <p class="text-[10px] text-muted-foreground mt-1 font-mono">{{ ev.created_at }}</p>
                    </div>
                  </div>
                  <!-- Pagination -->
                  <div v-if="eventsTotalPages > 1" class="flex items-center justify-center gap-1 pt-2 pb-1">
                    <button
                      class="px-2 py-1 rounded text-[10px] transition-colors"
                      :class="eventsPage === 1 ? 'text-muted-foreground/40 cursor-default' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
                      :disabled="eventsPage === 1"
                      @click="eventsGo(eventsPage - 1)"
                    >Prev</button>
                    <button
                      v-for="p in Math.min(eventsTotalPages, 5)"
                      :key="'ep-'+p"
                      class="w-6 h-6 rounded text-[10px] tabular-nums transition-colors"
                      :class="p === eventsPage ? 'bg-primary text-primary-foreground font-semibold' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
                      @click="eventsGo(p)"
                    >{{ p }}</button>
                    <button
                      class="px-2 py-1 rounded text-[10px] transition-colors"
                      :class="eventsPage === eventsTotalPages ? 'text-muted-foreground/40 cursor-default' : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
                      :disabled="eventsPage === eventsTotalPages"
                      @click="eventsGo(eventsPage + 1)"
                    >Next</button>
                  </div>
                </div>
              </div>
            </template>

          </div>
        </div>
      </div>
    </Teleport>

    <!-- ── Create dialog ── -->
    <Teleport to="body">
      <div v-if="showCreate" class="fixed inset-0 z-50 flex items-start justify-center pt-[15vh]" @click.self="showCreate = false">
        <div class="soft-panel w-full max-w-md p-6 space-y-5">
          <div class="flex items-center justify-between">
            <h2 class="text-base font-semibold tracking-tight">New Runner</h2>
            <button type="button" class="inline-flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-muted hover:text-foreground" @click="showCreate = false"><X class="h-4 w-4" /></button>
          </div>

          <template v-if="!createdToken">
            <div class="space-y-4">
              <div>
                <label class="type-label block mb-1.5">Name</label>
                <input v-model="newName" type="text" placeholder="e.g. prod-runner-01" class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15" @keyup.enter="handleCreate" />
              </div>
              <div>
                <label class="type-label block mb-1.5">Labels (comma-separated)</label>
                <input v-model="newLabels" type="text" placeholder="linux-x64, prod" class="w-full rounded-xl border border-border bg-input px-3 py-2 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15" @keyup.enter="handleCreate" />
              </div>
            </div>
            <div class="flex justify-end gap-2 pt-1">
              <UiButton variant="outline" @click="showCreate = false">Cancel</UiButton>
              <UiButton :disabled="creating || !newName.trim()" @click="handleCreate">{{ creating ? 'Creating...' : 'Create Runner' }}</UiButton>
            </div>
          </template>

          <template v-else>
            <div class="space-y-4">
              <div class="rounded-xl bg-muted px-4 py-3">
                <p class="type-eyebrow mb-1.5">Registration Token</p>
                <p class="font-mono text-sm break-all text-foreground">{{ createdToken }}</p>
              </div>
              <p class="type-meta leading-relaxed">
                Copy this token into your Runner's <code class="font-mono bg-muted px-1 rounded">conf/runner.toml</code>
                <code class="block mt-1.5 font-mono bg-muted px-1 rounded">register_token = "this-token"</code>
              </p>
              <div class="flex gap-2">
                <UiButton variant="outline" class="flex-1" @click="copyToken">
                  <Copy v-if="!copied" class="h-4 w-4" />
                  <Check v-else class="h-4 w-4" />
                  {{ copied ? 'Copied' : 'Copy Token' }}
                </UiButton>
                <UiButton variant="outline" @click="showCreate = false">Done</UiButton>
              </div>
            </div>
          </template>
        </div>
      </div>
    </Teleport>
  </div>
</template>
