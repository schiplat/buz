<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import UiPagination from '@/components/ui/UiPagination.vue'
import { Plus, Pencil, Trash2, Server } from '@lucide/vue'
import { hostsApi, type Host } from '@/api/hosts'

const hosts = ref<Host[]>([])
const loading = ref(true)
const hostPage = ref(1)
const pageSize = 15

// ── Create / Edit dialog ──
const showDialog = ref(false)
const editing = ref<Host | null>(null)
const form = ref({ name: '', host: '', port: 22, username: '', credential_type: 'key_path', credential_ref: '', labels: '' })
const saving = ref(false)

function openCreate() {
  editing.value = null
  form.value = { name: '', host: '', port: 22, username: '', credential_type: 'key_path', credential_ref: '', labels: '' }
  showDialog.value = true
}

function openEdit(h: Host) {
  editing.value = h
  form.value = {
    name: h.name,
    host: h.host,
    port: h.port,
    username: h.username,
    credential_type: h.credential_type,
    credential_ref: h.credential_ref,
    labels: (h.labels || []).join(', '),
  }
  showDialog.value = true
}

async function handleSave() {
  saving.value = true
  try {
    const payload: any = {
      name: form.value.name.trim(),
      host: form.value.host.trim(),
      port: Number(form.value.port) || 22,
      username: form.value.username.trim(),
      credential_type: form.value.credential_type,
      credential_ref: form.value.credential_ref.trim(),
      labels: form.value.labels
        ? form.value.labels.split(',').map((s: string) => s.trim()).filter(Boolean)
        : [],
    }
    if (editing.value) {
      await hostsApi.update(editing.value.id, payload)
    } else {
      await hostsApi.create(payload)
    }
    showDialog.value = false
    await loadHosts()
  } finally {
    saving.value = false
  }
}

async function handleDelete(h: Host) {
  if (!confirm(`Delete host "${h.name}"? This cannot be undone.`)) return
  await hostsApi.delete(h.id)
  await loadHosts()
}

async function loadHosts() {
  try {
    hosts.value = await hostsApi.list()
  } catch {
    // ignore
  } finally {
    loading.value = false
  }
}

const pagedHosts = computed(() => {
  const start = (hostPage.value - 1) * pageSize
  return hosts.value.slice(start, start + pageSize)
})

onMounted(loadHosts)
</script>

<template>
  <div class="space-y-6">
    <PageHeader title="Hosts" description="SSH remote hosts for cross-network task execution.">
      <template #actions>
        <UiButton size="sm" @click="openCreate"><Plus class="h-4 w-4" /> Add Host</UiButton>
      </template>
    </PageHeader>

    <!-- Table -->
    <div v-if="loading" class="text-center py-12 text-muted-foreground text-sm">Loading…</div>
    <div v-else class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
      <div v-if="hosts.length > 0" class="overflow-x-auto">
        <table class="w-full text-[13px]">
          <thead>
            <tr class="border-b border-border/30 bg-muted/10">
              <th class="text-left px-5 py-2.5 font-medium text-muted-foreground text-[11px] uppercase tracking-wider">Name</th>
              <th class="text-left px-5 py-2.5 font-medium text-muted-foreground text-[11px] uppercase tracking-wider">Host</th>
              <th class="text-left px-5 py-2.5 font-medium text-muted-foreground text-[11px] uppercase tracking-wider">Port</th>
              <th class="text-left px-5 py-2.5 font-medium text-muted-foreground text-[11px] uppercase tracking-wider">User</th>
              <th class="text-left px-5 py-2.5 font-medium text-muted-foreground text-[11px] uppercase tracking-wider">Auth</th>
              <th class="text-left px-5 py-2.5 font-medium text-muted-foreground text-[11px] uppercase tracking-wider">Labels</th>
              <th class="text-right px-5 py-2.5 w-20" />
            </tr>
          </thead>
          <tbody class="divide-y divide-border/20">
            <tr v-for="h in pagedHosts" :key="h.id" class="hover:bg-muted/5 transition-colors">
              <td class="px-5 py-3 font-semibold whitespace-nowrap">{{ h.name }}</td>
              <td class="px-5 py-3 font-mono text-xs text-muted-foreground whitespace-nowrap">{{ h.host }}</td>
              <td class="px-5 py-3 font-mono text-xs text-muted-foreground whitespace-nowrap">{{ h.port }}</td>
              <td class="px-5 py-3 whitespace-nowrap">{{ h.username }}</td>
              <td class="px-5 py-3 whitespace-nowrap">
                <span class="inline-flex items-center rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide bg-slate-500/10 text-slate-500">{{ h.credential_type }}</span>
              </td>
              <td class="px-5 py-3">
                <div class="flex flex-wrap gap-1">
                  <span v-for="l in h.labels" :key="l" class="mono-chip text-[10px]">{{ l }}</span>
                  <span v-if="!h.labels?.length" class="text-muted-foreground/40 text-xs">—</span>
                </div>
              </td>
              <td class="px-5 py-3 text-right whitespace-nowrap">
                <div class="flex items-center justify-end gap-1">
                  <UiButton variant="ghost" size="sm" @click="openEdit(h)" title="Edit"><Pencil class="h-3.5 w-3.5" /></UiButton>
                  <UiButton variant="ghost" size="sm" @click="handleDelete(h)" title="Delete"><Trash2 class="h-3.5 w-3.5 text-red-500" /></UiButton>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
        <div class="px-5 py-2 border-t border-border/20">
          <UiPagination v-model:page="hostPage" :total="hosts.length" :page-size="pageSize" />
        </div>
      </div>
      <div v-else class="flex flex-col items-center justify-center py-16 text-muted-foreground">
        <Server class="h-10 w-10 mb-3 text-muted-foreground/20" />
        <p class="text-sm">No hosts configured yet</p>
        <p class="text-xs mt-1 text-muted-foreground/60">Add an SSH host to enable remote step execution.</p>
      </div>
    </div>

    <!-- Dialog overlay -->
    <Teleport to="body">
      <div v-if="showDialog" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="showDialog = false" />
        <div class="relative z-10 w-full max-w-lg mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6">
          <h3 class="text-[15px] font-semibold mb-5">{{ editing ? 'Edit Host' : 'New Host' }}</h3>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Name <span class="text-red-400">*</span></label>
              <input v-model="form.name" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20" placeholder="e.g. prod-server-01" />
            </div>
            <div class="grid grid-cols-2 gap-3">
              <div>
                <label class="block text-[12px] font-medium text-foreground mb-1">Address <span class="text-red-400">*</span></label>
                <input v-model="form.host" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20" placeholder="10.0.0.1" />
              </div>
              <div>
                <label class="block text-[12px] font-medium text-foreground mb-1">Port</label>
                <input v-model.number="form.port" type="number" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20" placeholder="22" />
              </div>
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Username <span class="text-red-400">*</span></label>
              <input v-model="form.username" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20" placeholder="root" />
            </div>
            <div class="grid grid-cols-2 gap-3">
              <div>
                <label class="block text-[12px] font-medium text-foreground mb-1">Auth Type</label>
                <select v-model="form.credential_type" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20">
                  <option value="key_path">SSH Key</option>
                  <option value="password">Password</option>
                </select>
              </div>
              <div>
                <label class="block text-[12px] font-medium text-foreground mb-1">Credential <span class="text-red-400">*</span></label>
                <input v-model="form.credential_ref" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20" :placeholder="form.credential_type === 'key_path' ? '~/.ssh/id_rsa' : '$SECRET_NAME'" />
              </div>
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Labels <span class="text-muted-foreground/60 font-normal">(comma-separated)</span></label>
              <input v-model="form.labels" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20" placeholder="linux, x64, prod" />
            </div>
          </div>
          <div class="flex items-center justify-end gap-3 mt-6 pt-5 border-t border-border/30">
            <UiButton variant="ghost" size="sm" @click="showDialog = false" :disabled="saving">Cancel</UiButton>
            <UiButton size="sm" @click="handleSave" :disabled="saving">{{ saving ? 'Saving…' : (editing ? 'Update' : 'Create') }}</UiButton>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>
