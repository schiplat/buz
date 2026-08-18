<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import TablePagination from '@/components/ui/TablePagination.vue'
import { Plus, Pencil, Trash2, SlidersHorizontal } from '@lucide/vue'
import { variablesApi, type Variable } from '@/api/variables'
import { useClientSort } from '@/composables/useClientSort'
import { useClientPagination } from '@/composables/useClientPagination'

const variables = ref<Variable[]>([])
const loading = ref(true)
const searchQuery = ref('')

const filteredVars = computed(() => {
  const q = searchQuery.value.trim().toLowerCase()
  if (!q) return variables.value
  return variables.value.filter((v) => {
    return [v.name, v.value].filter(Boolean).join(' ').toLowerCase().includes(q)
  })
})

const { sorted, toggleSort, sortIndicator } = useClientSort(filteredVars, {
  initialKey: 'name',
  initialDir: 'asc',
  getValue: (row, key) => {
    switch (key) {
      case 'name': return row.name
      case 'value': return row.value
      case 'updated_at': return row.updated_at || ''
      default: return ''
    }
  },
})

const { page, pageSize, pageCount, total, pageItems, rangeLabel } = useClientPagination(sorted)

// ── Create / Edit dialog ──
const showDialog = ref(false)
const editing = ref<Variable | null>(null)
const formName = ref('')
const formValue = ref('')
const saving = ref(false)

function openCreate() {
  editing.value = null
  formName.value = ''
  formValue.value = ''
  showDialog.value = true
}

function openEdit(v: Variable) {
  editing.value = v
  formName.value = v.name
  formValue.value = ''
  showDialog.value = true
}

async function handleSave() {
  saving.value = true
  try {
    if (editing.value) {
      await variablesApi.update(editing.value.id, { value: formValue.value })
    } else {
      await variablesApi.create({ name: formName.value.trim(), value: formValue.value })
    }
    showDialog.value = false
    await loadVars()
  } catch (e: any) {
    // Surface the backend's message instead of letting the unhandled rejection
    // crash the page via ErrorBoundary (same pattern as Secrets.vue).
    alert(e?.response?.data?.message || e?.message || 'Failed to save variable')
  } finally {
    saving.value = false
  }
}

async function handleDelete(v: Variable) {
  if (!confirm(`Delete variable "${v.name}"?`)) return
  try {
    await variablesApi.delete(v.id)
    await loadVars()
  } catch (e: any) {
    alert(e?.response?.data?.message || e?.message || 'Failed to delete variable')
  }
}

async function loadVars() {
  try {
    variables.value = await variablesApi.list()
  } catch {
    // ignore
  } finally {
    loading.value = false
  }
}

onMounted(loadVars)
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      title="Variables"
      description="Non-sensitive platform config. Bind to a pipeline via Advanced → Variables; available as $NAME in every step. Never store passwords here."
    >
      <template #actions>
        <UiButton size="sm" @click="openCreate"><Plus class="h-4 w-4" /> Add Variable</UiButton>
      </template>
    </PageHeader>

    <div v-if="loading" class="text-center py-12 text-muted-foreground text-sm">Loading…</div>
    <div v-else class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
      <div v-if="variables.length === 0" class="flex flex-col items-center justify-center py-16 text-muted-foreground">
        <SlidersHorizontal class="h-10 w-10 mb-3 text-muted-foreground/20" />
        <p class="text-sm">No variables configured yet</p>
        <p class="text-xs mt-1 text-muted-foreground/60">
          After creating variables, bind them in Pipeline → Advanced → Variables
          (or YAML <code class="mono-chip text-[10px]">variables: [NAME]</code>).
        </p>
      </div>
      <template v-else>
        <!-- Filter bar -->
        <div class="px-5 py-3 border-b border-border/30 flex items-center gap-3">
          <input
            v-model="searchQuery"
            type="text"
            placeholder="Search variables…"
            class="w-full max-w-56 rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50 focus:ring-1 focus:ring-primary/10"
          />
          <span v-if="filteredVars.length !== variables.length" class="text-[10px] text-muted-foreground ml-auto">
            {{ filteredVars.length }} of {{ variables.length }}
          </span>
        </div>
        <div class="overflow-x-auto">
          <table class="w-full text-[13px]">
            <thead>
              <tr class="border-b border-border/30 bg-muted/10">
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('name')">
                    Name
                    <span class="font-mono text-xs">{{ sortIndicator('name') || '↕' }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('value')">
                    Value
                    <span class="font-mono text-xs">{{ sortIndicator('value') || '↕' }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('updated_at')">
                    Updated
                    <span class="font-mono text-xs">{{ sortIndicator('updated_at') || '↕' }}</span>
                  </button>
                </th>
                <th class="text-right px-5 py-2.5 w-20" />
              </tr>
            </thead>
            <tbody class="divide-y divide-border/20">
              <tr v-for="v in pageItems" :key="v.id" class="hover:bg-muted/5 transition-colors">
                <td class="px-5 py-3 font-mono text-sm font-semibold whitespace-nowrap">{{ v.name }}</td>
                <td class="px-5 py-3">
                  <code class="mono-chip text-xs">{{ v.value }}</code>
                </td>
                <td class="px-5 py-3 text-xs text-muted-foreground whitespace-nowrap">{{ v.updated_at?.slice(0, 10) || '—' }}</td>
                <td class="px-5 py-3 text-right whitespace-nowrap">
                  <div class="flex items-center justify-end gap-1">
                    <UiButton variant="ghost" size="sm" @click="openEdit(v)" title="Edit"><Pencil class="h-3.5 w-3.5" /></UiButton>
                    <UiButton variant="ghost" size="sm" @click="handleDelete(v)" title="Delete"><Trash2 class="h-3.5 w-3.5 text-red-500" /></UiButton>
                  </div>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <TablePagination v-model:page="page" v-model:pageSize="pageSize" :total="total" :page-count="pageCount" :range-label="rangeLabel" />
      </template>
    </div>

    <!-- Dialog -->
    <Teleport to="body">
      <div v-if="showDialog" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="showDialog = false" />
        <div class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6">
          <h3 class="text-[15px] font-semibold mb-5">{{ editing ? 'Edit Variable' : 'New Variable' }}</h3>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">Name <span class="text-red-500">*</span></label>
              <input
                v-model="formName"
                :disabled="!!editing"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20 disabled:bg-muted/30 disabled:cursor-not-allowed"
                placeholder="DOCKER_REGISTRY"
              />
            </div>
            <div>
              <label class="block text-[12px] font-medium text-foreground mb-1">
                Value <span class="text-red-500">*</span>
                <span v-if="editing" class="text-muted-foreground font-normal ml-1">(leave empty to keep current)</span>
              </label>
              <textarea
                v-model="formValue"
                rows="6"
                spellcheck="false"
                class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono outline-none focus:border-primary/50 focus:ring-1 focus:ring-primary/20 resize-y min-h-[7.5rem]"
                :placeholder="editing ? '(keep current value)' : 'e.g. registry.example.com'"
              />
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
