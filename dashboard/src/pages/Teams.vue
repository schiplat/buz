<script setup lang="ts">
import { ref, onMounted } from 'vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import { Plus, Trash2, Users } from '@lucide/vue'
import { teamsApi, type Team, type TeamMember } from '@/api/teams'
import { usersApi, type User } from '@/api/users'
import { useAuthStore } from '@/stores/auth'
import { useAppDialog } from '@/composables/useAppDialog'

const auth = useAuthStore()
const dialog = useAppDialog()
const teams = ref<Team[]>([])
const users = ref<User[]>([])
const loading = ref(true)

const showCreate = ref(false)
const formName = ref('')
const formSlug = ref('')
const formDesc = ref('')
const saving = ref(false)

const selected = ref<Team | null>(null)
const members = ref<TeamMember[]>([])
const memberUserId = ref('')
const memberRole = ref('developer')

async function load() {
  try {
    teams.value = await teamsApi.list()
    if (auth.isPlatformAdmin) {
      users.value = await usersApi.list()
    }
  } catch {
    // ignore
  } finally {
    loading.value = false
  }
}

async function openTeam(t: Team) {
  selected.value = await teamsApi.get(t.id)
  members.value = selected.value.members || []
}

async function createTeam() {
  if (!formName.value.trim() || !formSlug.value.trim()) {
    await dialog.alert('Name and slug are required')
    return
  }
  saving.value = true
  try {
    await teamsApi.create({
      name: formName.value.trim(),
      slug: formSlug.value.trim().toLowerCase(),
      description: formDesc.value.trim() || undefined,
    })
    showCreate.value = false
    formName.value = ''
    formSlug.value = ''
    formDesc.value = ''
    await load()
  } finally {
    saving.value = false
  }
}

async function removeTeam(t: Team) {
  if (!(await dialog.confirm(`Delete team "${t.name}"?`, {
    title: 'Delete team',
    confirmLabel: 'Delete',
    danger: true,
  }))) return
  await teamsApi.delete(t.id)
  if (selected.value?.id === t.id) selected.value = null
  await load()
}

async function addMember() {
  if (!selected.value || !memberUserId.value) return
  await teamsApi.upsertMember(selected.value.id, {
    user_id: memberUserId.value,
    role: memberRole.value,
  })
  await openTeam(selected.value)
}

async function removeMember(m: TeamMember) {
  if (!selected.value) return
  if (!(await dialog.confirm(`Remove ${m.username} from team?`, {
    title: 'Remove member',
    confirmLabel: 'Remove',
    danger: true,
  }))) return
  await teamsApi.removeMember(selected.value.id, m.user_id)
  await openTeam(selected.value)
}

onMounted(load)
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      title="Teams"
      description="Teams own pipelines and secrets. Members have roles: Owner, Maintainer, Developer, Viewer."
    >
      <template v-if="auth.isPlatformAdmin" #actions>
        <UiButton size="sm" @click="showCreate = true"><Plus class="h-4 w-4" /> New Team</UiButton>
      </template>
    </PageHeader>

    <div v-if="loading" class="text-center py-12 text-muted-foreground text-sm">Loading…</div>
    <div v-else class="grid gap-6 lg:grid-cols-2">
      <div class="rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm">
        <div v-if="teams.length === 0" class="flex flex-col items-center justify-center py-16 text-muted-foreground">
          <Users class="h-10 w-10 mb-3 text-muted-foreground/20" />
          <p class="text-sm">No teams yet</p>
        </div>
        <ul v-else class="divide-y divide-border/20">
          <li
            v-for="t in teams"
            :key="t.id"
            class="flex items-center justify-between px-5 py-3 hover:bg-muted/5 cursor-pointer"
            @click="openTeam(t)"
          >
            <div>
              <div
                class="font-mono text-[12px] font-medium leading-snug text-foreground/90 break-all"
                :title="t.name"
              >{{ t.name }}</div>
              <div class="text-xs text-muted-foreground font-mono">{{ t.slug }} · {{ t.my_role || 'admin' }}</div>
            </div>
            <UiButton
              v-if="auth.isPlatformAdmin || t.my_role === 'owner'"
              variant="ghost"
              size="sm"
              @click.stop="removeTeam(t)"
            >
              <Trash2 class="h-3.5 w-3.5 text-red-500" />
            </UiButton>
          </li>
        </ul>
      </div>

      <div v-if="selected" class="rounded-xl border border-border/50 bg-card p-5 shadow-sm space-y-4">
        <div>
          <h3 class="font-semibold">{{ selected.name }}</h3>
          <p class="text-xs text-muted-foreground font-mono">{{ selected.slug }}</p>
        </div>
        <div>
          <h4 class="text-[12px] font-medium mb-2">Members</h4>
          <ul class="space-y-2 mb-4">
            <li
              v-for="m in members"
              :key="m.user_id"
              class="flex items-center justify-between text-sm"
            >
              <span>{{ m.username }} <span class="text-xs text-muted-foreground">({{ m.role }})</span></span>
              <UiButton variant="ghost" size="sm" @click="removeMember(m)">
                <Trash2 class="h-3.5 w-3.5 text-red-500" />
              </UiButton>
            </li>
          </ul>
          <div v-if="auth.isPlatformAdmin || selected.my_role === 'owner'" class="flex gap-2">
            <select v-model="memberUserId" class="flex-1 rounded-lg border border-border/60 bg-background px-2 py-1.5 text-sm">
              <option value="">Select user</option>
              <option v-for="u in users" :key="u.id" :value="u.id">{{ u.username }}</option>
            </select>
            <select v-model="memberRole" class="rounded-lg border border-border/60 bg-background px-2 py-1.5 text-sm">
              <option value="owner">owner</option>
              <option value="maintainer">maintainer</option>
              <option value="developer">developer</option>
              <option value="viewer">viewer</option>
            </select>
            <UiButton size="sm" @click="addMember">Add</UiButton>
          </div>
        </div>
      </div>
    </div>

    <Teleport to="body">
      <div v-if="showCreate" class="fixed inset-0 z-50 flex items-center justify-center">
        <div class="absolute inset-0 bg-black/30 backdrop-blur-sm" @click="showCreate = false" />
        <div class="relative z-10 w-full max-w-md mx-4 rounded-2xl border border-border/50 bg-card shadow-2xl p-6">
          <h3 class="text-[15px] font-semibold mb-5">New Team</h3>
          <div class="space-y-4">
            <div>
              <label class="block text-[12px] font-medium mb-1">Name <span class="text-red-500">*</span></label>
              <input v-model="formName" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm" placeholder="Backend" />
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Slug <span class="text-red-500">*</span></label>
              <input v-model="formSlug" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm font-mono" placeholder="backend" />
            </div>
            <div>
              <label class="block text-[12px] font-medium mb-1">Description</label>
              <input v-model="formDesc" class="w-full rounded-lg border border-border/60 bg-background px-3 py-2 text-sm" />
            </div>
          </div>
          <div class="flex justify-end gap-3 mt-6 pt-5 border-t border-border/30">
            <UiButton variant="ghost" size="sm" @click="showCreate = false">Cancel</UiButton>
            <UiButton size="sm" :disabled="saving" @click="createTeam">{{ saving ? 'Creating…' : 'Create' }}</UiButton>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>
