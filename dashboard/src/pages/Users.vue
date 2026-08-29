<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import TablePagination from '@/components/ui/TablePagination.vue'
import { Plus, Trash2, UserRound } from '@lucide/vue'
import { usersApi, type User } from '@/api/users'
import { useAuthStore } from '@/stores/auth'
import { useRouter } from 'vue-router'
import { useClientSort } from '@/composables/useClientSort'
import { useClientPagination } from '@/composables/useClientPagination'
import { useAppDialog } from '@/composables/useAppDialog'

const auth = useAuthStore()
const router = useRouter()
const dialog = useAppDialog()
const users = ref<User[]>([])
const loading = ref(true)
const searchQuery = ref('')

const filteredUsers = computed(() => {
  const q = searchQuery.value.trim().toLowerCase()
  if (!q) return users.value
  return users.value.filter((u) => {
    return [u.username, u.display_name, u.is_platform_admin ? 'admin' : 'member', u.is_super_admin ? 'super' : '', u.is_active ? 'active' : 'disabled']
      .filter(Boolean)
      .join(' ')
      .toLowerCase()
      .includes(q)
  })
})

const { sorted, toggleSort, sortIndicator } = useClientSort(filteredUsers, {
  initialKey: 'username',
  initialDir: 'asc',
  getValue: (row, key) => {
    switch (key) {
      case 'username': return row.username
      case 'role': return row.is_super_admin ? 2 : row.is_platform_admin ? 1 : 0
      case 'status': return row.is_active ? 1 : 0
      default: return ''
    }
  },
})

const { page, pageSize, pageCount, total, pageItems, rangeLabel } = useClientPagination(sorted)

const showCreate = ref(false)
const formUsername = ref('')
const formPassword = ref('')
const formDisplay = ref('')
const formAdmin = ref(false)
const formSuper = ref(false)
const saving = ref(false)

async function load() {
  if (!auth.isPlatformAdmin) {
    router.replace('/dashboard')
    return
  }
  try {
    users.value = await usersApi.list()
  } catch {
    // ignore
  } finally {
    loading.value = false
  }
}

async function createUser() {
  if (!formUsername.value.trim() || !formPassword.value) {
    await dialog.alert('Username and password are required')
    return
  }
  saving.value = true
  try {
    await usersApi.create({
      username: formUsername.value.trim(),
      password: formPassword.value,
      display_name: formDisplay.value.trim() || undefined,
      is_platform_admin: formAdmin.value,
      is_super_admin: auth.isSuperAdmin ? formSuper.value : undefined,
    })
    showCreate.value = false
    formUsername.value = ''
    formPassword.value = ''
    formDisplay.value = ''
    formAdmin.value = false
    formSuper.value = false
    await load()
  } finally {
    saving.value = false
  }
}

async function toggleActive(u: User) {
  await usersApi.update(u.id, { is_active: !u.is_active })
  await load()
}

async function toggleSuper(u: User) {
  const action = u.is_super_admin ? 'demote from super admin' : 'promote to super admin'
  if (!(await dialog.confirm(`${action[0].toUpperCase() + action.slice(1)} "${u.username}"?`, {
    title: 'Change role',
    confirmLabel: 'Confirm',
    danger: true,
  }))) return
  await usersApi.update(u.id, { is_super_admin: !u.is_super_admin })
  await load()
}

async function deleteUser(u: User) {
  if (!(await dialog.confirm(`Delete user "${u.username}"?`, {
    title: 'Delete user',
    confirmLabel: 'Delete',
    danger: true,
  }))) return
  await usersApi.delete(u.id)
  await load()
}

onMounted(load)
</script>

<template>
  <div class="space-y-6">
    <PageHeader title="Users" description="Platform users. Only platform admins can manage accounts.">
      <template #actions>
        <UiButton size="sm" @click="showCreate = true"><Plus class="h-4 w-4" /> New User</UiButton>
      </template>
    </PageHeader>

    <div v-if="loading" class="text-center py-12 text-muted-foreground text-sm">Loading…</div>
    <div v-else class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
      <div v-if="users.length === 0" class="flex flex-col items-center justify-center py-16 text-muted-foreground">
        <UserRound class="h-10 w-10 mb-3 text-muted-foreground/20" />
        <p class="text-sm">No users</p>
      </div>
      <template v-else>
        <!-- Search bar -->
        <div class="px-5 py-3 border-b border-border/30">
          <input
            v-model="searchQuery"
            type="text"
            placeholder="Search users…"
            class="w-full max-w-xs rounded-lg border border-border/60 bg-background px-3 py-1.5 text-xs outline-none transition-colors focus:border-primary/50 focus:ring-1 focus:ring-primary/10"
          />
        </div>
        <div class="overflow-x-auto">
          <table class="w-full text-[13px]">
            <thead>
              <tr class="border-b border-border/30 bg-muted/10">
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('username')">
                    Username
                    <span class="font-mono text-xs">{{ sortIndicator('username') || '↕' }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('role')">
                    Role
                    <span class="font-mono text-xs">{{ sortIndicator('role') || '↕' }}</span>
                  </button>
                </th>
                <th class="text-left px-5 py-2.5">
                  <button type="button" class="inline-flex items-center gap-1 text-[11px] uppercase text-muted-foreground hover:text-foreground transition-colors" @click="toggleSort('status')">
                    Status
                    <span class="font-mono text-xs">{{ sortIndicator('status') || '↕' }}</span>
                  </button>
                </th>
                <th class="text-right px-5 py-2.5 w-28" />
              </tr>
            </thead>
            <tbody class="divide-y divide-border/20">
              <tr v-for="u in pageItems" :key="u.id" class="hover:bg-muted/5">
                <td class="px-5 py-3 font-semibold">{{ u.username }}</td>
                <td class="px-5 py-3 text-xs">
                  <span v-if="u.is_super_admin" class="font-semibold text-amber-500">super admin</span>
                  <span v-else-if="u.is_platform_admin">platform admin</span>
                  <span v-else>member</span>
                </td>
                <td class="px-5 py-3 text-xs">{{ u.is_active ? 'active' : 'disabled' }}</td>
                <td class="px-5 py-3 text-right space-x-1">
                  <UiButton v-if="auth.isSuperAdmin && u.id !== auth.userId" variant="ghost" size="sm" @click="toggleSuper(u)">
                    {{ u.is_super_admin ? 'Demote' : 'Promote' }}
                  </UiButton>
                  <UiButton variant="ghost" size="sm" @click="toggleActive(u)">
                    {{ u.is_active ? 'Disable' : 'Enable' }}
                  </UiButton>
                  <UiButton variant="ghost" size="sm" @click="deleteUser(u)">
                    <Trash2 class="h-3.5 w-3.5 text-red-500" />
                  </UiButton>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <TablePagination v-model:page="page" v-model:pageSize="pageSize" :total="total" :page-count="pageCount" :range-label="rangeLabel" />
      </template>
    </div>

    <Teleport to="body">
      <div v-if="showCreate" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="showCreate = false" />
        <div class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6">
          <h3 class="text-[15px] font-semibold mb-5">New User</h3>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium mb-1">Username <span class="text-red-500">*</span></label>
              <input v-model="formUsername" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm" />
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Password <span class="text-red-500">*</span></label>
              <input v-model="formPassword" type="password" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm" />
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Display name</label>
              <input v-model="formDisplay" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm" />
            </div>
            <label class="flex items-center gap-2 text-sm">
              <input v-model="formAdmin" type="checkbox" class="rounded" />
              Platform admin
            </label>
            <label v-if="auth.isSuperAdmin" class="flex items-center gap-2 text-sm">
              <input v-model="formSuper" type="checkbox" class="rounded" />
              Super admin (sees all API tokens &amp; audit logs)
            </label>
          </div>
          <div class="flex justify-end gap-3 mt-6 pt-5 border-t border-border/30">
            <UiButton variant="ghost" size="sm" @click="showCreate = false">Cancel</UiButton>
            <UiButton size="sm" :disabled="saving" @click="createUser">{{ saving ? 'Creating…' : 'Create' }}</UiButton>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>
