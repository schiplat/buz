<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { Server, Box, Play } from '@lucide/vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import BuildTrendChart from '@/components/dashboard/BuildTrendChart.vue'
import BuildStreamCard from '@/components/dashboard/BuildStreamCard.vue'
import { runnersApi, type Runner } from '@/api/runners'
import { pipelinesApi, type Pipeline } from '@/api/pipelines'
import { jobsApi, type Job, type JobTrend, type RecentJob } from '@/api/jobs'
import { useAuthStore } from '@/stores/auth'

const auth = useAuthStore()

const runners = ref<Runner[]>([])
const pipelines = ref<Pipeline[]>([])
const jobs = ref<Job[]>([])
const recentJobs = ref<RecentJob[]>([])
const trend = ref<JobTrend>({ days: 7, points: [] })
const trendDays = ref(7)
const loading = ref(true)
const onlineCount = ref(0)

/** Active team scope for the job stats; empty = all visible teams. */
function activeTeamId(): string | undefined {
  return auth.activeTeamId || undefined
}

// Stats load is resilient: a failing API (e.g. runners for non-admins) must not
// blank out the whole dashboard — every card degrades independently.
async function loadStats() {
  loading.value = true
  const teamId = activeTeamId()
  const [r, p, j, rec, tr] = await Promise.allSettled([
    runnersApi.list(),
    pipelinesApi.list(),
    jobsApi.list(),
    jobsApi.recent({ limit: 10, teamId }),
    jobsApi.trend({ days: trendDays.value, teamId }),
  ])
  if (r.status === 'fulfilled') {
    runners.value = r.value
    onlineCount.value = r.value.filter((runner) => runner.status === 'online').length
  } else {
    runners.value = []
    onlineCount.value = 0
  }
  if (p.status === 'fulfilled') pipelines.value = p.value
  if (j.status === 'fulfilled') jobs.value = j.value
  if (rec.status === 'fulfilled') recentJobs.value = rec.value
  if (tr.status === 'fulfilled') trend.value = tr.value
  loading.value = false
}

async function loadTrend() {
  try {
    trend.value = await jobsApi.trend({ days: trendDays.value, teamId: activeTeamId() })
  } catch (e) {
    console.error('Failed to load build trend', e)
  }
}

function onTrendDaysChange(days: number) {
  trendDays.value = days
  loadTrend()
}

/** Jobs of the active team only (backend already scopes to visible teams). */
const visibleJobs = computed(() => jobs.value.filter((j) => auth.matchesActiveTeam(j.team_id)))

watch(() => auth.activeTeamId, () => loadStats())

onMounted(loadStats)
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      title="Overview"
      description="System overview and key metrics of your Buz deployment."
    />

    <div class="grid gap-6 xl:grid-cols-[minmax(0,1fr)_330px]">
      <div class="min-w-0 space-y-6">
        <div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          <div class="surface-card p-5">
            <div class="flex items-start justify-between gap-3">
              <div>
                <p class="type-eyebrow">Runners</p>
                <p class="mt-2 text-lg font-semibold tracking-tight">
                  <template v-if="loading">—</template>
                  <template v-else>{{ onlineCount }} / {{ runners.length }}</template>
                </p>
                <p class="mt-1.5 type-meta">Online / Total</p>
              </div>
              <span class="flex h-9 w-9 items-center justify-center rounded-xl bg-muted text-muted-foreground">
                <Server class="h-4 w-4" />
              </span>
            </div>
          </div>

          <div class="surface-card p-5">
            <div class="flex items-start justify-between gap-3">
              <div>
                <p class="type-eyebrow">Pipelines</p>
                <p class="mt-2 text-lg font-semibold tracking-tight">
                  <template v-if="loading">—</template>
                  <template v-else>{{ pipelines.length }}</template>
                </p>
                <p class="mt-1.5 type-meta">Active configurations</p>
              </div>
              <span class="flex h-9 w-9 items-center justify-center rounded-xl bg-muted text-muted-foreground">
                <Box class="h-4 w-4" />
              </span>
            </div>
          </div>

          <div class="surface-card p-5">
            <div class="flex items-start justify-between gap-3">
              <div>
                <p class="type-eyebrow">Recent Jobs</p>
                <p class="mt-2 text-lg font-semibold tracking-tight">
                  <template v-if="loading">—</template>
                  <template v-else>{{ visibleJobs.length }}</template>
                </p>
                <p class="mt-1.5 type-meta">Total executions</p>
              </div>
              <span class="flex h-9 w-9 items-center justify-center rounded-xl bg-muted text-muted-foreground">
                <Play class="h-4 w-4" />
              </span>
            </div>
          </div>
        </div>

        <BuildTrendChart :points="trend.points" :days="trendDays" @update:days="onTrendDaysChange" />
      </div>

      <aside class="min-w-0">
        <BuildStreamCard :jobs="recentJobs" :loading="loading" />
      </aside>
    </div>
  </div>
</template>
