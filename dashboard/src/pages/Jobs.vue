<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import PageHeader from '@/components/ui/PageHeader.vue'
import UiButton from '@/components/ui/UiButton.vue'
import StatusBadge from '@/components/ui/StatusBadge.vue'
import { jobsApi, type Job } from '@/api/jobs'
import { XCircle, Info, Lock } from '@lucide/vue'
import { useAuthStore } from '@/stores/auth'

const router = useRouter()
const auth = useAuthStore()
const jobs = ref<Job[]>([])
const loading = ref(true)

const filteredJobs = computed(() =>
  jobs.value.filter((j) => auth.matchesActiveTeam(j.team_id)),
)

onMounted(async () => {
  await loadJobs()
})

async function loadJobs() {
  loading.value = true
  try {
    jobs.value = await jobsApi.list()
  } catch (e) {
    console.error(e)
  } finally {
    loading.value = false
  }
}

async function handleCancel(jobId: string) {
  try {
    await jobsApi.cancel(jobId)
    await loadJobs()
  } catch (e) {
    console.error(e)
  }
}
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      title="Jobs"
      description="Execution history for all pipeline runs."
    />

    <div v-if="loading" class="surface-card p-8 text-center type-meta">Loading...</div>

    <template v-else>
      <div v-if="filteredJobs.length === 0" class="surface-card p-8 text-center type-meta">
        {{ jobs.length === 0 ? 'No jobs executed yet' : 'No jobs in the selected team' }}
      </div>

      <div v-else class="surface-card data-table-scroll">
        <table class="data-table">
          <thead>
            <tr>
              <th>Pipeline</th>
              <th>Status</th>
              <th>Mode</th>
              <th>Runner</th>
              <th>Started</th>
              <th>Duration</th>
              <th />
            </tr>
          </thead>
          <tbody>
            <tr v-for="j in filteredJobs" :key="j.id" class="clickable">
              <td class="font-medium">{{ j.pipeline_name || j.pipeline_id }}</td>
              <td>
                <StatusBadge :status="j.status" />
              </td>
              <td>
                <div class="flex items-center gap-1.5">
                  <span
                    v-if="j.verbose_logs"
                    class="inline-flex items-center gap-1 rounded-full border border-sky-500/30 bg-sky-500/10 px-2 py-0.5 text-[11px] font-medium text-sky-600"
                    title="Verbose mode: extra per-step detail in logs; secrets stay redacted."
                  ><Info class="h-3 w-3" />Verbose</span>
                  <span
                    v-if="j.debug_unredact"
                    class="inline-flex items-center gap-1 rounded-full border border-amber-500/40 bg-amber-500/10 px-2 py-0.5 text-[11px] font-medium text-amber-700"
                    title="Debug mode: logs are unredacted, platform-admin-only, and purged after 24h."
                  ><Lock v-if="!auth.isPlatformAdmin" class="h-3 w-3" />Debug</span>
                  <span v-if="!j.verbose_logs && !j.debug_unredact" class="text-xs text-muted-foreground/40">—</span>
                </div>
              </td>
              <td class="font-mono text-xs text-muted-foreground">{{ j.runner_id || '—' }}</td>
              <td class="text-xs text-muted-foreground">{{ j.start_time || '—' }}</td>
              <td class="text-xs text-muted-foreground">{{ j.end_time || '—' }}</td>
              <td class="text-right">
                <div class="flex items-center justify-end gap-1">
                  <UiButton v-if="j.status === 'running' || j.status === 'pending' || j.status === 'waiting_approval' || j.status === 'queued'" variant="ghost" size="sm" @click="handleCancel(j.id)">
                    <XCircle class="h-3.5 w-3.5 text-red-500" />
                    Cancel
                  </UiButton>
                  <UiButton variant="ghost" size="sm" @click="router.push(`/jobs/${j.id}`)">View</UiButton>
                </div>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </template>
  </div>
</template>
