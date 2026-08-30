<script setup lang="ts">
import { computed, ref } from 'vue'
import { useElementSize } from '@vueuse/core'
import type { DailyBuildPoint } from '@/api/jobs'

const props = defineProps<{
  points: DailyBuildPoint[]
  days: number
}>()

const emit = defineEmits<{
  (e: 'update:days', days: number): void
}>()

const DAY_OPTIONS = [1, 7, 15, 30] as const

const containerRef = ref<HTMLElement | null>(null)
const { width } = useElementSize(containerRef)

const HEIGHT = 184
const PAD = { top: 14, right: 8, bottom: 26, left: 34 }

const chartW = computed(() => Math.max(0, width.value))
const innerW = computed(() => Math.max(40, chartW.value - PAD.left - PAD.right))
const innerH = computed(() => HEIGHT - PAD.top - PAD.bottom)

const hasData = computed(() => props.points.some((p) => p.total > 0))

const daysLabel = computed(() => `${props.days} ${props.days === 1 ? 'day' : 'days'}`)

function niceCeil(v: number): number {
  if (v <= 1) return 1
  const pow = Math.pow(10, Math.floor(Math.log10(v)))
  const n = v / pow
  if (n <= 1) return pow
  if (n <= 2) return 2 * pow
  if (n <= 5) return 5 * pow
  return 10 * pow
}

const maxTotal = computed(() =>
  props.points.reduce((acc, p) => Math.max(acc, p.total), 0),
)
const niceMax = computed(() => niceCeil(maxTotal.value))

function yScale(v: number): number {
  return PAD.top + innerH.value * (1 - v / niceMax.value)
}

const slotW = computed(() => innerW.value / Math.max(1, props.points.length))
const barW = computed(() => Math.min(22, Math.max(3, slotW.value * 0.55)))

function xFor(i: number): number {
  return PAD.left + slotW.value * i + slotW.value / 2
}

const GRID_TICKS = [0, 0.25, 0.5, 0.75, 1] as const

const labelStep = computed(() =>
  Math.max(1, Math.ceil(props.points.length / 6)),
)

function showXLabel(i: number): boolean {
  return i % labelStep.value === 0 || i === props.points.length - 1
}

function shortDate(date: string): string {
  const [, m, d] = date.split('-')
  return `${m}-${d}`
}

const SEGMENT_ORDER = ['success', 'failed', 'cancelled', 'other'] as const

const SEGMENT_COLORS: Record<(typeof SEGMENT_ORDER)[number], string> = {
  success: 'hsl(var(--success))',
  failed: 'hsl(var(--destructive))',
  cancelled: 'hsl(var(--muted-foreground) / 0.5)',
  other: 'hsl(var(--secondary-foreground) / 0.14)',
}

const SEGMENT_LABELS: Record<(typeof SEGMENT_ORDER)[number], string> = {
  success: 'Success',
  failed: 'Failed',
  cancelled: 'Cancelled',
  other: 'Other',
}

interface Segment {
  key: (typeof SEGMENT_ORDER)[number]
  value: number
  y0: number
  y1: number
  color: string
}

/** Series hidden via legend clicks — kept out of the stacked bars. */
const hiddenSegments = ref<Set<(typeof SEGMENT_ORDER)[number]>>(new Set())

function isHidden(key: (typeof SEGMENT_ORDER)[number]): boolean {
  return hiddenSegments.value.has(key)
}

function toggleSegment(key: (typeof SEGMENT_ORDER)[number]) {
  const next = new Set(hiddenSegments.value)
  if (next.has(key)) {
    next.delete(key)
  } else {
    next.add(key)
  }
  hiddenSegments.value = next
}

const allHidden = computed(() => SEGMENT_ORDER.every((k) => hiddenSegments.value.has(k)))

function barSegments(p: DailyBuildPoint): Segment[] {
  let acc = 0
  const segs: Segment[] = []
  for (const key of SEGMENT_ORDER) {
    if (hiddenSegments.value.has(key)) continue
    const value = Math.max(0, p[key] || 0)
    const y0 = yScale(acc)
    acc += value
    const y1 = yScale(acc)
    if (value > 0) {
      segs.push({ key, value, y0, y1, color: SEGMENT_COLORS[key] })
    }
  }
  return segs
}

/** Sum of the visible series only — matches the stacked bar height. */
function visibleTotal(p: DailyBuildPoint): number {
  return SEGMENT_ORDER.reduce((acc, key) => {
    if (hiddenSegments.value.has(key)) return acc
    return acc + Math.max(0, p[key] || 0)
  }, 0)
}

function tooltip(p: DailyBuildPoint): string {
  return [
    `${p.date} · ${p.total} build${p.total === 1 ? '' : 's'}`,
    p.success > 0 ? `${p.success} success` : '',
    p.failed > 0 ? `${p.failed} failed` : '',
    p.cancelled > 0 ? `${p.cancelled} cancelled` : '',
  ]
    .filter(Boolean)
    .join(' · ')
}
</script>

<template>
  <div class="surface-card p-5">
    <div class="flex items-center justify-between gap-3">
      <div class="min-w-0">
        <p class="type-eyebrow">Builds</p>
        <p class="mt-1 text-sm font-semibold tracking-tight">Daily build trend</p>
      </div>
      <div class="flex shrink-0 items-center gap-0.5 rounded-lg border border-border bg-muted/40 p-0.5">
        <button
          v-for="d in DAY_OPTIONS"
          :key="d"
          type="button"
          class="rounded-md px-2.5 py-1 text-xs font-medium transition-colors"
          :class="days === d ? 'bg-card text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'"
          @click="emit('update:days', d)"
        >
          {{ d }}d
        </button>
      </div>
    </div>

    <div ref="containerRef" class="mt-4">
      <div v-if="!hasData" class="flex h-[184px] items-center justify-center text-sm text-muted-foreground">
        No builds in the last {{ daysLabel }}
      </div>
      <div
        v-else-if="allHidden"
        class="flex h-[184px] items-center justify-center text-sm text-muted-foreground"
      >
        All series hidden — click a legend item to show it
      </div>
      <svg
        v-else
        :viewBox="`0 0 ${chartW} ${HEIGHT}`"
        :width="chartW"
        :height="HEIGHT"
        class="overflow-visible"
        role="img"
        :aria-label="`Daily build trend for the last ${daysLabel}`"
      >
        <!-- Y grid lines + labels -->
        <template v-for="t in GRID_TICKS" :key="t">
          <line
            :x1="PAD.left"
            :x2="chartW - PAD.right"
            :y1="yScale(niceMax * t)"
            :y2="yScale(niceMax * t)"
            stroke="hsl(var(--border))"
            stroke-width="1"
            stroke-dasharray="3 4"
          />
          <text
            :x="PAD.left - 6"
            :y="yScale(niceMax * t) + 3.5"
            text-anchor="end"
            class="fill-muted-foreground"
            style="font-size: 10px; font-family: inherit"
          >
            {{ Math.round(niceMax * t) }}
          </text>
        </template>

        <!-- Stacked bars -->
        <g v-for="(p, i) in points" :key="p.date">
          <g
            v-for="seg in barSegments(p)"
            :key="seg.key"
            :transform="`translate(${xFor(i) - barW / 2} 0)`"
          >
            <rect
              :x="0"
              :y="seg.y0"
              :width="barW"
              :height="Math.max(0, seg.y1 - seg.y0)"
              :rx="i === points.length - 1 ? 2 : 1.5"
              :fill="seg.color"
              :fill-opacity="seg.key === 'other' ? 1 : 0.95"
            >
              <title>{{ tooltip(p) }}</title>
            </rect>
          </g>
          <!-- Total count above bar for short ranges -->
          <text
            v-if="points.length <= 7 && visibleTotal(p) > 0"
            :x="xFor(i)"
            :y="yScale(visibleTotal(p)) - 5"
            text-anchor="middle"
            class="fill-muted-foreground"
            style="font-size: 10px; font-family: inherit"
          >
            {{ visibleTotal(p) }}
          </text>
          <!-- X labels -->
          <text
            v-if="showXLabel(i)"
            :x="xFor(i)"
            :y="HEIGHT - 8"
            text-anchor="middle"
            class="fill-muted-foreground"
            style="font-size: 10px; font-family: inherit"
          >
            {{ shortDate(p.date) }}
          </text>
        </g>
      </svg>
    </div>

    <div class="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1.5">
      <button
        v-for="key in SEGMENT_ORDER"
        :key="key"
        type="button"
        class="inline-flex items-center gap-1.5 text-[11px] font-medium transition-opacity"
        :class="isHidden(key) ? 'text-muted-foreground/45' : 'text-muted-foreground hover:text-foreground'"
        :title="isHidden(key) ? `Show ${SEGMENT_LABELS[key]}` : `Hide ${SEGMENT_LABELS[key]}`"
        @click="toggleSegment(key)"
      >
        <span
          class="h-2 w-2 rounded-full transition-opacity"
          :class="isHidden(key) && 'opacity-40'"
          :style="{ background: SEGMENT_COLORS[key] }"
        />
        <span :class="isHidden(key) && 'line-through decoration-muted-foreground/40'">
          {{ SEGMENT_LABELS[key] }}
        </span>
      </button>
    </div>
  </div>
</template>
