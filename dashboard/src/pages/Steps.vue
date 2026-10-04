<script setup lang="ts">
import { ref, computed, onMounted } from 'vue'
import PageHeader from '@/components/ui/PageHeader.vue'
import { Search, Loader2, ExternalLink, ShieldAlert, TriangleAlert } from '@lucide/vue'
import { stepsApi, type StepTemplate, type StepLibrary } from '@/api/steps'

const loading = ref(true)
const library = ref<StepLibrary>({ items: [], unparseable: [] })
const searchQuery = ref('')
const selected = ref<StepTemplate | null>(null)

const filtered = computed(() => {
  const q = searchQuery.value.trim().toLowerCase()
  if (!q) return library.value.items
  return library.value.items.filter((t) => {
    const haystack = [
      t.id,
      t.name,
      t.description,
      t.description_zh,
      t.author,
      t.engine,
      ...(t.keywords || []),
    ]
      .filter(Boolean)
      .join(' ')
      .toLowerCase()
    return haystack.includes(q)
  })
})

function usageSnippet(t: StepTemplate): string {
  const lines = [`- name: ${t.id.split('/').pop() || t.id}`, `  uses: ${t.id}@${t.version}`]
  const required = t.inputs.filter((i) => i.required)
  if (required.length) {
    lines.push('  with:')
    for (const input of required) lines.push(`    ${input.name}: `)
  }
  return lines.join('\n')
}

const reportUrl = (t: StepTemplate) =>
  `https://github.com/schiplat/buz-plugins/issues/new?template=report.yml&title=${encodeURIComponent(
    `[report] ${t.id}@${t.version}`,
  )}`

onMounted(async () => {
  try {
    library.value = await stepsApi.list()
  } finally {
    loading.value = false
  }
})
</script>

<template>
  <div class="max-w-6xl mx-auto px-4 sm:px-6 py-6">
    <PageHeader title="Step Library" description="Reusable step templates available to this Manager. Reference them from pipelines with `uses:`." />

    <div v-if="library.unparseable.length" class="mb-4 rounded-lg border border-amber-500/30 bg-amber-500/5 p-3 text-xs text-amber-700 dark:text-amber-400">
      <div class="flex items-center gap-1.5 font-medium mb-1">
        <TriangleAlert class="h-3.5 w-3.5" />
        {{ library.unparseable.length }} file(s) in the step directory failed to parse
      </div>
      <div v-for="u in library.unparseable" :key="u.file" class="font-mono">{{ u.file }} — {{ u.error }}</div>
    </div>

    <div class="relative mb-4">
      <Search class="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
      <input
        v-model="searchQuery"
        type="text"
        placeholder="Search by name, id, keyword, author…"
        class="w-full rounded-lg border border-border bg-input pl-9 pr-3 py-2 text-sm outline-none focus:border-ring/50"
      />
    </div>

    <div v-if="loading" class="flex items-center justify-center py-16 text-muted-foreground">
      <Loader2 class="h-5 w-5 animate-spin mr-2" /> Loading step library…
    </div>

    <div v-else-if="!filtered.length" class="text-center py-16 text-muted-foreground text-sm">
      <template v-if="searchQuery">No steps match “{{ searchQuery }}”.</template>
      <template v-else>
        Step library is empty — clone
        <a
          href="https://github.com/schiplat/buz-plugins"
          target="_blank"
          class="text-primary hover:underline inline-flex items-center gap-0.5"
        >schiplat/buz-plugins <ExternalLink class="h-3 w-3" /></a>
        into the Manager's step directory to install templates.
      </template>
    </div>

    <div v-else class="grid gap-3 sm:grid-cols-2">
      <button
        v-for="t in filtered"
        :key="`${t.id}@${t.version}`"
        type="button"
        class="text-left rounded-xl border bg-card p-4 hover:border-ring/50 transition-colors"
        :class="selected?.id === t.id && selected?.version === t.version ? 'border-ring' : 'border-border/60'"
        @click="selected = t"
      >
        <div class="flex items-start justify-between gap-2">
          <div>
            <div class="font-mono text-sm font-medium text-foreground">{{ t.id }}</div>
            <div class="text-xs text-muted-foreground mt-0.5">v{{ t.version }} · {{ t.engine }}</div>
          </div>
          <span
            class="rounded px-1.5 py-0.5 text-[10px] font-medium"
            :class="t.publisher === 'official'
              ? 'bg-emerald-500/10 text-emerald-600 border border-emerald-500/20'
              : 'bg-sky-500/10 text-sky-600 border border-sky-500/20'"
          >{{ t.publisher }}</span>
        </div>
        <p class="text-xs text-muted-foreground mt-2 line-clamp-2">
          {{ t.description_zh || t.description || '—' }}
        </p>
        <div class="flex items-center gap-2 mt-2 flex-wrap">
          <span v-if="t.author" class="text-[10px] text-muted-foreground">by {{ t.author }}</span>
          <span v-if="!t.buz_compatible" class="inline-flex items-center gap-1 text-[10px] text-destructive">
            <ShieldAlert class="h-3 w-3" /> requires newer Buz
          </span>
        </div>
      </button>
    </div>

    <div v-if="selected" class="mt-6 rounded-xl border border-border bg-card p-5">
      <div class="flex items-start justify-between gap-3">
        <div>
          <div class="font-mono text-sm font-medium">{{ selected.id }}@{{ selected.version }}</div>
          <div class="text-xs text-muted-foreground mt-1">{{ selected.description || selected.description_zh }}</div>
        </div>
        <a
          :href="reportUrl(selected)"
          target="_blank"
          class="shrink-0 text-[11px] text-muted-foreground hover:text-destructive inline-flex items-center gap-1"
        >
          <ShieldAlert class="h-3 w-3" /> Report
        </a>
      </div>

      <div class="mt-4">
        <div class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground mb-1.5">Usage</div>
        <pre class="rounded-lg bg-muted/60 p-3 text-xs font-mono overflow-x-auto">{{ usageSnippet(selected) }}</pre>
      </div>

      <div v-if="selected.inputs.length" class="mt-4">
        <div class="text-[11px] font-medium uppercase tracking-wide text-muted-foreground mb-1.5">Inputs</div>
        <table class="w-full text-xs">
          <thead>
            <tr class="text-left text-muted-foreground border-b border-border/60">
              <th class="py-1.5 pr-3 font-medium">Name</th>
              <th class="py-1.5 pr-3 font-medium">Type</th>
              <th class="py-1.5 pr-3 font-medium">Required</th>
              <th class="py-1.5 pr-3 font-medium">Masking</th>
              <th class="py-1.5 font-medium">Description</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="input in selected.inputs" :key="input.name" class="border-b border-border/30 last:border-0">
              <td class="py-1.5 pr-3 font-mono">{{ input.name }}</td>
              <td class="py-1.5 pr-3">{{ input.type }}</td>
              <td class="py-1.5 pr-3">{{ input.required ? 'yes' : 'no' }}</td>
              <td class="py-1.5 pr-3">{{ input.masking ? 'yes' : 'no' }}</td>
              <td class="py-1.5 text-muted-foreground">{{ input.description || '—' }}</td>
            </tr>
          </tbody>
        </table>
      </div>

      <div v-if="selected.source_repo" class="mt-4 text-xs">
        <a :href="selected.source_repo" target="_blank" class="text-primary hover:underline inline-flex items-center gap-0.5">
          Source repository <ExternalLink class="h-3 w-3" />
        </a>
      </div>
    </div>
  </div>
</template>
