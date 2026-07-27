<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { Server, Box, Play } from '@lucide/vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import { runnersApi, type Runner } from '@/api/runners'
import { pipelinesApi, type Pipeline } from '@/api/pipelines'
import { jobsApi, type Job } from '@/api/jobs'

const runners = ref<Runner[]>([])
const pipelines = ref<Pipeline[]>([])
const jobs = ref<Job[]>([])
const loading = ref(true)
const onlineCount = ref(0)

onMounted(async () => {
  try {
    const [r, p, j] = await Promise.all([
      runnersApi.list(),
      pipelinesApi.list(),
      jobsApi.list(),
    ])
    runners.value = r
    pipelines.value = p
    jobs.value = j
    onlineCount.value = r.filter(runner => runner.status === 'online').length
  } catch (e) {
    console.error('Failed to load dashboard', e)
  } finally {
    loading.value = false
  }
})
</script>

<template>
  <div class="space-y-6">
    <PageHeader
      title="Overview"
      description="System overview and key metrics of your Buz deployment."
    />

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
              <template v-else>{{ jobs.length }}</template>
            </p>
            <p class="mt-1.5 type-meta">Total executions</p>
          </div>
          <span class="flex h-9 w-9 items-center justify-center rounded-xl bg-muted text-muted-foreground">
            <Play class="h-4 w-4" />
          </span>
        </div>
      </div>
    </div>
  </div>
</template>
