<script setup lang="ts">
import { ref, watch, nextTick, markRaw, computed, onMounted, onUnmounted } from 'vue'
import { VueFlow, useVueFlow } from '@vue-flow/core'
import { Background } from '@vue-flow/background'
import { Controls } from '@vue-flow/controls'
import { Maximize2, Minimize2 } from '@lucide/vue'
import dagre from 'dagre'
import { useDark } from '@vueuse/core'
import StepNode from '@/components/flow/StepNode.vue'
import type { StepNodeData } from '@/components/flow/StepNode.vue'

import '@vue-flow/core/dist/style.css'
import '@vue-flow/core/dist/theme-default.css'
import '@vue-flow/controls/dist/style.css'

const props = defineProps<{
  tasks: { name: string; exec_type: string }[]
  stepStatuses: any[] | null
  fmtDuration: (idx: number, ss: any[] | null) => string
}>()

const emit = defineEmits<{
  (e: 'step-click', payload: { stepIndex: number; jobId: string }): void
}>()

const isDark = useDark({ selector: 'html', attribute: 'class', valueDark: 'dark', valueLight: 'light' })

const { fitView, onNodeClick } = useVueFlow()

const nodeTypes = {
  'step-node': markRaw(StepNode),
}

type FilterKey = 'all' | 'success' | 'failed' | 'pending'
const activeFilter = ref<FilterKey>('all')

const filterOptions: { key: FilterKey; label: string; dot: string }[] = [
  { key: 'all', label: 'All', dot: 'bg-slate-400' },
  { key: 'success', label: 'Success', dot: 'bg-emerald-500' },
  { key: 'failed', label: 'Failed', dot: 'bg-red-500' },
  { key: 'pending', label: 'Pending', dot: 'bg-slate-300' },
]

const nodes = ref<any[]>([])
const edges = ref<any[]>([])

// Fullscreen
const graphRoot = ref<HTMLElement | null>(null)
const isFullscreen = ref(false)

function toggleFullscreen() {
  const el = graphRoot.value
  if (!el) return
  if (document.fullscreenElement === el) {
    void document.exitFullscreen?.()
    return
  }
  void el.requestFullscreen?.().catch(() => {})
}

function onFullscreenChange() {
  isFullscreen.value = document.fullscreenElement === graphRoot.value
  nextTick(() => fitView({ padding: 0.15, duration: 200 }))
}

onMounted(() => {
  document.addEventListener('fullscreenchange', onFullscreenChange)
})
onUnmounted(() => {
  document.removeEventListener('fullscreenchange', onFullscreenChange)
})

const NODE_WIDTH = 170
const NODE_HEIGHT = 90
const PADDING_X = 40
const PADDING_Y = 40

function getStepStatus(stepIndex: number, stepStatuses: any[] | null): string {
  if (!stepStatuses) return 'pending'
  const found = stepStatuses.find((s: any) => Number(s.step_order) === stepIndex)
  return found?.status || 'pending'
}

// Filter counts
const filterCounts = computed(() => {
  const counts: Record<FilterKey, number> = { all: props.tasks.length, success: 0, failed: 0, pending: 0 }
  props.tasks.forEach((_, idx) => {
    const s = getStepStatus(idx, props.stepStatuses)
    if (s === 'success') counts.success++
    else if (s === 'failed') counts.failed++
    else counts.pending++
  })
  return counts
})

function buildGraph() {
  const rawNodes: any[] = []
  const rawEdges: any[] = []

  const ss = props.stepStatuses
  const filter = activeFilter.value

  const filteredIndices = new Set<number>()
  props.tasks.forEach((_, idx) => {
    const status = getStepStatus(idx, ss)
    if (filter === 'all') {
      filteredIndices.add(idx)
    } else if (filter === 'success' && status === 'success') {
      filteredIndices.add(idx)
    } else if (filter === 'failed' && (status === 'failed' || status === 'cancelled')) {
      filteredIndices.add(idx)
    } else if (filter === 'pending' && status === 'pending') {
      filteredIndices.add(idx)
    }
  })

  props.tasks.forEach((task, idx) => {
    const status = getStepStatus(idx, ss)
    const visible = filteredIndices.has(idx)

    rawNodes.push({
      id: `step-${idx}`,
      type: 'step-node',
      data: {
        stepIndex: idx,
        name: task.name,
        execType: task.exec_type,
        status,
        duration: props.fmtDuration(idx, ss),
      } as StepNodeData,
      position: { x: 0, y: 0 },
      hidden: !visible,
    })

    if (idx > 0 && visible && filteredIndices.has(idx - 1)) {
      const prevStatus = getStepStatus(idx - 1, ss)
      rawEdges.push({
        id: `e-step-${idx - 1}-step-${idx}`,
        source: `step-${idx - 1}`,
        target: `step-${idx}`,
        type: 'smoothstep',
        animated: prevStatus === 'running' || status === 'running',
        style: {
          stroke: status === 'success'
            ? '#34d399'
            : status === 'failed'
              ? '#f87171'
              : prevStatus === 'running' || status === 'running'
                ? '#fbbf24'
                : isDark.value ? '#52525b' : '#d4d4d8',
          strokeWidth: 1.5,
        },
      })
    }
  })

  // dagre layout (LR = left-to-right)
  const dagreGraph = new dagre.graphlib.Graph()
  dagreGraph.setDefaultEdgeLabel(() => ({}))
  dagreGraph.setGraph({ rankdir: 'LR', nodesep: 40, ranksep: 40 })

  rawNodes.forEach(n => {
    dagreGraph.setNode(n.id, { width: NODE_WIDTH, height: NODE_HEIGHT })
  })
  rawEdges.forEach(e => {
    dagreGraph.setEdge(e.source, e.target)
  })

  dagre.layout(dagreGraph)

  const layouted = rawNodes.map(n => {
    const pos = dagreGraph.node(n.id)
    return {
      ...n,
      position: {
        x: (pos.x - NODE_WIDTH / 2) + PADDING_X,
        y: (pos.y - NODE_HEIGHT / 2) + PADDING_Y,
      },
    }
  })

  nodes.value = layouted
  edges.value = rawEdges

  nextTick(() => {
    fitView({ padding: 0.15, duration: 200 })
  })
}

onNodeClick(({ node }) => {
  const data = node.data as StepNodeData
  if (data.status === 'pending') return
  emit('step-click', { stepIndex: data.stepIndex, jobId: '' })
})

function setFilter(key: FilterKey) {
  activeFilter.value = key
  buildGraph()
}

watch(
  () => [props.tasks, props.stepStatuses],
  () => buildGraph(),
  { deep: true, immediate: true },
)

watch(isDark, () => buildGraph())
watch(isFullscreen, () => buildGraph())

defineExpose({ refresh: buildGraph })
</script>

<template>
  <div
    ref="graphRoot"
    class="pipeline-topology h-[360px] w-full rounded-xl border border-border/50 bg-card overflow-hidden shadow-sm relative"
    :class="{ 'pipeline-topology--fullscreen': isFullscreen }"
  >
    <!-- Filter toolbar -->
    <div
      v-if="tasks.length > 0"
      class="absolute top-3 left-3 z-10 flex items-center gap-1.5 rounded-lg bg-card/95 border border-border/50 px-2 py-1.5 shadow-sm backdrop-blur"
    >
      <button
        v-for="opt in filterOptions"
        :key="opt.key"
        class="flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs font-medium transition-colors"
        :class="activeFilter === opt.key
          ? 'bg-foreground/10 text-foreground'
          : 'text-muted-foreground hover:text-foreground hover:bg-foreground/5'"
        @click="setFilter(opt.key)"
      >
        <span class="w-2 h-2 rounded-full shrink-0" :class="opt.dot" />
        <span>{{ opt.label }}</span>
        <span v-if="filterCounts[opt.key] > 0" class="tabular-nums text-xs opacity-60 ml-0.5">{{ filterCounts[opt.key] }}</span>
      </button>
    </div>

    <!-- Fullscreen toggle -->
    <button
      v-if="tasks.length > 0"
      class="absolute top-3 right-3 z-10 flex items-center justify-center w-7 h-7 rounded-lg bg-card/95 border border-border/50 shadow-sm backdrop-blur text-muted-foreground hover:text-foreground transition-colors"
      @click="toggleFullscreen"
      :title="isFullscreen ? 'Exit fullscreen' : 'Fullscreen'"
    >
      <Minimize2 v-if="isFullscreen" class="w-3.5 h-3.5" />
      <Maximize2 v-else class="w-3.5 h-3.5" />
    </button>

    <div v-if="tasks.length === 0" class="flex items-center justify-center h-full text-muted-foreground text-sm">
      No steps defined
    </div>
    <VueFlow
      v-else
      v-model:nodes="nodes"
      v-model:edges="edges"
      :node-types="nodeTypes as any"
      :default-viewport="{ zoom: 1, x: 0, y: 0 }"
      :min-zoom="0.15"
      :max-zoom="2"
      :nodes-draggable="false"
      :nodes-connectable="false"
      :elements-selectable="false"
      fit-view-on-init
    >
      <Background :pattern-color="isDark ? '#3f3f46' : '#e4e4e7'" :gap="20" :size="1" />
      <Controls class="!bg-card !border-border !shadow-lg !m-4 !rounded-lg overflow-hidden !bottom-4 !left-4" />
    </VueFlow>
  </div>
</template>

<style>
.pipeline-topology .vue-flow__node-step-node {
  width: auto !important;
  height: auto !important;
  padding: 0 !important;
  border: none !important;
  background: transparent !important;
  box-shadow: none !important;
}

.pipeline-topology .vue-flow__edge-path {
  transition: stroke 0.3s;
}

.pipeline-topology:fullscreen,
.pipeline-topology.pipeline-topology--fullscreen {
  width: 100%;
  height: 100%;
  background: hsl(var(--background));
}

.dark .pipeline-topology:fullscreen,
.dark .pipeline-topology.pipeline-topology--fullscreen {
  background: hsl(var(--background));
}
</style>
