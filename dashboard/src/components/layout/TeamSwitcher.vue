<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { Users } from '@lucide/vue'
import { useAuthStore } from '@/stores/auth'
import { teamsApi, type Team } from '@/api/teams'

const auth = useAuthStore()
const allTeams = ref<Team[]>([])

const options = computed(() => {
  if (auth.isPlatformAdmin && allTeams.value.length > 0) {
    return allTeams.value.map((t) => ({ id: t.id, label: `${t.name} (${t.slug})` }))
  }
  return auth.teams.map((t) => ({ id: t.id, label: `${t.name} (${t.slug})` }))
})

async function loadAdminTeams() {
  if (!auth.isPlatformAdmin) {
    allTeams.value = []
    return
  }
  try {
    allTeams.value = await teamsApi.list()
  } catch {
    allTeams.value = []
  }
}

onMounted(loadAdminTeams)
watch(() => auth.isPlatformAdmin, loadAdminTeams)

function onChange(e: Event) {
  const v = (e.target as HTMLSelectElement).value
  auth.setActiveTeamId(v)
}
</script>

<template>
  <div
    v-if="auth.showTeamSwitcher"
    class="flex items-center gap-1.5 rounded-full border border-border/60 bg-muted/30 py-1 pl-2.5 pr-2"
    title="Filter lists by team"
  >
    <Users class="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
    <select
      class="max-w-42 cursor-pointer appearance-none bg-transparent text-xs font-medium text-foreground outline-hidden"
      :value="auth.activeTeamId"
      aria-label="Active team filter"
      @change="onChange"
    >
      <option value="">All my teams</option>
      <option v-for="t in options" :key="t.id" :value="t.id">{{ t.label }}</option>
    </select>
  </div>
</template>
