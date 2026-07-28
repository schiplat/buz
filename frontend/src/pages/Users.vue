<script setup lang="ts">
import { ref, onMounted } from 'vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import { Plus, Trash2, UserRound } from '@lucide/vue'
import { usersApi, type User } from '@/api/users'
import { useAuthStore } from '@/stores/auth'
import { useRouter } from 'vue-router'

const auth = useAuthStore()
const router = useRouter()
const users = ref<User[]>([])
const loading = ref(true)
const showCreate = ref(false)
const formUsername = ref('')
const formPassword = ref('')
const formDisplay = ref('')
const formAdmin = ref(false)
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
    alert('Username and password are required')
    return
  }
  saving.value = true
  try {
    await usersApi.create({
      username: formUsername.value.trim(),
      password: formPassword.value,
      display_name: formDisplay.value.trim() || undefined,
      is_platform_admin: formAdmin.value,
    })
    showCreate.value = false
    formUsername.value = ''
    formPassword.value = ''
    formDisplay.value = ''
    formAdmin.value = false
    await load()
  } finally {
    saving.value = false
  }
}

async function toggleActive(u: User) {
  await usersApi.update(u.id, { is_active: !u.is_active })
  await load()
}

async function deleteUser(u: User) {
  if (!confirm(`Delete user "${u.username}"?`)) return
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
      <table v-else class="w-full text-[13px]">
        <thead>
          <tr class="border-b border-border/30 bg-muted/10">
            <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Username</th>
            <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Role</th>
            <th class="text-left px-5 py-2.5 text-[11px] uppercase text-muted-foreground">Status</th>
            <th class="text-right px-5 py-2.5 w-28" />
          </tr>
        </thead>
        <tbody class="divide-y divide-border/20">
          <tr v-for="u in users" :key="u.id" class="hover:bg-muted/5">
            <td class="px-5 py-3 font-semibold">{{ u.username }}</td>
            <td class="px-5 py-3 text-xs">{{ u.is_platform_admin ? 'platform admin' : 'member' }}</td>
            <td class="px-5 py-3 text-xs">{{ u.is_active ? 'active' : 'disabled' }}</td>
            <td class="px-5 py-3 text-right space-x-1">
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
