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

const SERIES_ORDER = ['success', 'failed', 'pending', 'cancelled', 'other'] as const
type SeriesKey = (typeof SERIES_ORDER)[number]

const SERIES_COLORS: Record<SeriesKey, string> = {
  success: '#10b981',
  failed: '#ef4444',
  pending: '#f59e0b',
  cancelled: '#3b82f6',
  other: '#a855f7',
}

const SERIES_LABELS: Record<SeriesKey, string> = {
  success: 'Success',
  failed: 'Failed',
  pending: 'Pending',
  cancelled: 'Cancelled',
  other: 'Other',
}

/** Series hidden via legend clicks — kept out of the chart. */
const hiddenSeries = ref<Set<SeriesKey>>(new Set())

function isHidden(key: SeriesKey): boolean {
  return hiddenSeries.value.has(key)
}

function toggleSeries(key: SeriesKey) {
  const next = new Set(hiddenSeries.value)
  if (next.has(key)) {
    next.delete(key)
  } else {
    next.add(key)
  }
  hiddenSeries.value = next
}

const visibleSeries = computed(() => SERIES_ORDER.filter((k) => !hiddenSeries.value.has(k)))
const allHidden = computed(() => visibleSeries.value.length === 0)

/** Y-axis ceiling from the visible series only — a hidden series cannot stretch it. */
const visibleMax = computed(() => {
  let m = 0
  for (const p of props.points) {
    for (const k of visibleSeries.value) {
      m = Math.max(m, p[k] || 0)
    }
  }
  return m
})
const niceMax = computed(() => niceCeil(visibleMax.value))

function yScale(v: number): number {
  return PAD.top + innerH.value * (1 - v / niceMax.value)
}

const slotW = computed(() => innerW.value / Math.max(1, props.points.length))

function xFor(i: number): number {
  return PAD.left + slotW.value * i + slotW.value / 2
}

const GRID_TICKS = [0, 0.25, 0.5, 0.75, 1] as const

const labelStep = computed(() => Math.max(1, Math.ceil(props.points.length / 6)))

function showXLabel(i: number): boolean {
  return i % labelStep.value === 0 || i === props.points.length - 1
}

function shortDate(date: string): string {
  const [, m, d] = date.split('-')
  return `${m}-${d}`
}

/** Catmull-Rom → cubic Bézier smooth curve through every daily point, giving
 *  rounded transitions at the inflection points. Zero-value points stay sharp:
 *  segments touching a zero are straight lines, so no rounded corner forms at
 *  the bottom of the chart. */
function seriesPath(key: SeriesKey): string {
  const pts = props.points.map((p, i) => ({
    x: xFor(i),
    y: yScale(p[key] || 0),
    value: p[key] || 0,
  }))
  if (pts.length === 0) return ''
  if (pts.length === 1) return `M ${pts[0].x} ${pts[0].y}`
  let d = `M ${pts[0].x} ${pts[0].y}`
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[Math.max(0, i - 1)]
    const p1 = pts[i]
    const p2 = pts[i + 1]
    const p3 = pts[Math.min(pts.length - 1, i + 2)]
    if (p1.value === 0 || p2.value === 0) {
      d += ` L ${p2.x} ${p2.y}`
    } else {
      const c1x = p1.x + (p2.x - p0.x) / 6
      const c1y = p1.y + (p2.y - p0.y) / 6
      const c2x = p2.x - (p3.x - p1.x) / 6
      const c2y = p2.y - (p3.y - p1.y) / 6
      d += ` C ${c1x} ${c1y}, ${c2x} ${c2y}, ${p2.x} ${p2.y}`
    }
  }
  return d
}

function pointTooltip(p: DailyBuildPoint): string {
  const parts = [`${p.date} · ${p.total} build${p.total === 1 ? '' : 's'}`]
  for (const k of visibleSeries.value) {
    const v = p[k] || 0
    if (v > 0) parts.push(`${v} ${SERIES_LABELS[k].toLowerCase()}`)
  }
  return parts.join(' · ')
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
          :class="days === d ? 'bg-card text-foreground shadow-xs' : 'text-muted-foreground hover:text-foreground'"
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

        <!-- Series lines + points -->
        <g v-for="key in visibleSeries" :key="key">
          <path
            :d="seriesPath(key)"
            fill="none"
            :stroke="SERIES_COLORS[key]"
            stroke-width="2.25"
            stroke-linecap="round"
            stroke-linejoin="round"
            :opacity="key === 'other' ? 0.75 : 1"
          />
          <g v-for="(p, i) in points" :key="p.date">
            <circle
              v-if="(p[key] || 0) > 0"
              :cx="xFor(i)"
              :cy="yScale(p[key] || 0)"
              r="3.5"
              :fill="SERIES_COLORS[key]"
              stroke="hsl(var(--card))"
              stroke-width="1.5"
            >
              <title>{{ pointTooltip(p) }}</title>
            </circle>
          </g>
        </g>

        <!-- X labels -->
        <g v-for="(p, i) in points" :key="p.date">
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
        v-for="key in SERIES_ORDER"
        :key="key"
        type="button"
        class="inline-flex items-center gap-1.5 text-[11px] font-medium transition-opacity"
        :class="isHidden(key) ? 'text-muted-foreground/45' : 'text-muted-foreground hover:text-foreground'"
        :title="isHidden(key) ? `Show ${SERIES_LABELS[key]}` : `Hide ${SERIES_LABELS[key]}`"
        @click="toggleSeries(key)"
      >
        <span
          class="h-2 w-2 rounded-full transition-opacity"
          :class="isHidden(key) && 'opacity-40'"
          :style="{ background: SERIES_COLORS[key] }"
        />
        <span :class="isHidden(key) && 'line-through decoration-muted-foreground/40'">
          {{ SERIES_LABELS[key] }}
        </span>
      </button>
    </div>
  </div>
</template>
