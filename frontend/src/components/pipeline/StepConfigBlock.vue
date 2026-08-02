<script setup lang="ts">
import { computed, watch } from 'vue'
import { Plus, Minus } from '@lucide/vue'
import {
  applyConfigMode,
  detectConfigMode,
  emptyRenderRow,
  normalizeConfigRender,
  stepAllowsConfigDeliver,
  stepAllowsConfigRender,
  type ConfigFormMode,
} from '@/utils/configForm'

const props = defineProps<{
  step: Record<string, any>
  sourceOptions: { id: string; label: string }[]
}>()

const uses = computed(() => (props.step.uses || 'shell') as string)
const allowDeliver = computed(() => stepAllowsConfigDeliver(uses.value))
const allowRender = computed(() => stepAllowsConfigRender(uses.value))

const mode = computed<ConfigFormMode>({
  get: () => detectConfigMode(props.step, allowRender.value),
  set: (m) => applyConfigMode(props.step, m, allowRender.value),
})

const modeOptions = computed(() => {
  const opts: { value: ConfigFormMode; label: string }[] = [
    { value: 'none', label: 'None' },
  ]
  if (allowDeliver.value) {
    opts.push({ value: 'deliver', label: 'OSS deliver' })
  }
  if (allowRender.value) {
    opts.push({ value: 'render', label: 'Render only' })
    opts.push({ value: 'sugar', label: 'Deliver + render (one step)' })
  }
  return opts
})

const showDeliver = computed(() => mode.value === 'deliver' || mode.value === 'sugar')
const showRender = computed(() => mode.value === 'render' || mode.value === 'sugar')
const showDest = computed(() => mode.value !== 'none')

const deliverRequired = computed(
  () => !!(props.step.config_source || props.step.config_key_path || props.step.config_dest),
)
const renderRequired = computed(() => showRender.value)

watch(
  () => props.step.config_render,
  (raw) => {
    if (showRender.value && !Array.isArray(raw)) {
      props.step.config_render = normalizeConfigRender(raw)
      if (props.step.config_render.length === 0) {
        props.step.config_render = [emptyRenderRow()]
      }
    }
  },
  { immediate: true },
)

watch(uses, () => {
  if (!allowRender.value && normalizeConfigRender(props.step.config_render).some((r) => r.template)) {
    props.step.config_render = []
    if (mode.value === 'render' || mode.value === 'sugar') {
      applyConfigMode(props.step, props.step.config_source || props.step.config_key_path ? 'deliver' : 'none', false)
    }
  }
})

function addRenderRow() {
  if (!Array.isArray(props.step.config_render)) {
    props.step.config_render = []
  }
  props.step.config_render.push(emptyRenderRow())
}

function removeRenderRow(idx: number) {
  if (!Array.isArray(props.step.config_render)) return
  props.step.config_render.splice(idx, 1)
  if (props.step.config_render.length === 0) {
    props.step.config_render = [emptyRenderRow()]
  }
}
</script>

<template>
  <div v-if="allowDeliver" class="space-y-2">
    <label class="text-xs font-medium text-muted-foreground uppercase tracking-wider mb-1 block">
      Config
      <span v-if="mode !== 'none'" class="text-red-500">*</span>
    </label>

    <select
      v-model="mode"
      class="w-full max-w-xs rounded-lg border border-border bg-input px-3 py-1.5 text-sm outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
    >
      <option v-for="opt in modeOptions" :key="opt.value" :value="opt.value">
        {{ opt.label }}
      </option>
    </select>

    <div v-if="showDeliver" class="grid grid-cols-2 gap-3">
      <div>
        <label class="text-[11px] text-muted-foreground mb-0.5 block">
          Source <span v-if="deliverRequired" class="text-red-500">*</span>
        </label>
        <select
          v-model="step.config_source"
          class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-sm font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
        >
          <option value="">Select OSS secret…</option>
          <option v-for="opt in sourceOptions" :key="opt.id" :value="opt.id">
            {{ opt.label }}
          </option>
        </select>
      </div>
      <div>
        <label class="text-[11px] text-muted-foreground mb-0.5 block">
          Key path <span v-if="deliverRequired" class="text-red-500">*</span>
        </label>
        <input
          v-model="step.config_key_path"
          type="text"
          placeholder="e.g. config_tpl/super-manager/$tag"
          class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-sm font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
        />
      </div>
    </div>

    <div v-if="showDest">
      <label class="text-[11px] text-muted-foreground mb-0.5 block">
        Dest <span class="text-red-500">*</span>
        <span class="normal-case font-normal text-muted-foreground/70"> → $BUZ_CONFIG_DIR</span>
      </label>
      <input
        v-model="step.config_dest"
        type="text"
        placeholder="e.g. /opt/manager"
        class="w-full rounded-lg border border-border bg-input px-3 py-1.5 text-sm font-mono outline-none transition-colors focus:border-ring/40 focus:ring-2 focus:ring-ring/15"
      />
    </div>

    <div v-if="showRender" class="space-y-1.5">
      <div class="flex items-center justify-between">
        <label class="text-[11px] text-muted-foreground block">
          Render <span v-if="renderRequired" class="text-red-500">*</span>
          <span class="normal-case font-normal text-muted-foreground/70">
            — relative to dest
          </span>
        </label>
        <button
          type="button"
          class="inline-flex items-center gap-1 text-[11px] text-primary hover:underline"
          @click="addRenderRow"
        >
          <Plus class="w-3 h-3" /> Add file
        </button>
      </div>
      <div class="rounded-lg border border-border overflow-hidden">
        <div
          class="grid grid-cols-[1fr_1fr_72px_72px_28px] gap-0 text-[10px] font-medium uppercase tracking-wider text-muted-foreground bg-muted/30 border-b border-border px-2 py-1.5"
        >
          <span>Template</span>
          <span>Output</span>
          <span>Mode</span>
          <span>Missing</span>
          <span />
        </div>
        <div
          v-for="(row, idx) in step.config_render || []"
          :key="idx"
          class="grid grid-cols-[1fr_1fr_72px_72px_28px] gap-0 border-b border-border/40 last:border-b-0"
        >
          <input
            v-model="row.template"
            type="text"
            placeholder="env.example"
            class="w-full bg-transparent px-2 py-1.5 text-xs font-mono outline-none border-r border-border/30"
          />
          <input
            v-model="row.output"
            type="text"
            placeholder=".env"
            class="w-full bg-transparent px-2 py-1.5 text-xs font-mono outline-none border-r border-border/30"
          />
          <input
            v-model="row.mode"
            type="text"
            placeholder="0600"
            class="w-full bg-transparent px-2 py-1.5 text-xs font-mono outline-none border-r border-border/30"
          />
          <select
            v-model="row.on_missing"
            class="w-full bg-transparent px-1 py-1.5 text-xs outline-none border-r border-border/30"
          >
            <option value="fail">fail</option>
            <option value="warn">warn</option>
          </select>
          <button
            type="button"
            class="flex items-center justify-center text-muted-foreground/50 hover:text-red-500 transition-colors"
            title="Remove render entry"
            @click="removeRenderRow(Number(idx))"
          >
            <Minus class="w-3.5 h-3.5" />
          </button>
        </div>
      </div>
    </div>

    <p class="text-xs text-muted-foreground/70 mt-0.5 leading-relaxed">
      Nested <code class="font-mono">config:</code> —
      prefer <strong>OSS deliver</strong> then a separate <strong>Render only</strong> step.
      Source secrets must be Kind = Aliyun OSS auth. Manager signs
      <code class="font-mono">key_path + /configs.tar.gz</code>; Runner unpacks / renders under
      <code class="font-mono">dest</code>.
      <span v-if="!allowRender"> Render is not available for Docker steps.</span>
    </p>
  </div>
</template>
