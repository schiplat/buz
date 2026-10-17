/** Helpers for nested `config:` form fields (flat API: config_source / key_path / dest / render). */

export interface ConfigRenderRow {
  template: string
  output: string
  mode: string
  on_missing: string
  backup_keep: number
}

export type ConfigFormMode = 'none' | 'deliver' | 'render' | 'sugar'

export function emptyRenderRow(): ConfigRenderRow {
  return {
    template: '',
    output: '.env',
    mode: '0600',
    on_missing: 'fail',
    backup_keep: 3,
  }
}

function trimStr(v: unknown): string {
  return typeof v === 'string' ? v.trim() : ''
}

/** Normalize API / YAML `config_render` into editable rows. */
export function normalizeConfigRender(raw: unknown): ConfigRenderRow[] {
  let value = raw
  if (typeof value === 'string' && value.trim()) {
    try {
      value = JSON.parse(value)
    } catch {
      return []
    }
  }
  if (value == null || value === '') return []
  const list = Array.isArray(value) ? value : [value]
  return list
    .filter((e) => e && typeof e === 'object')
    .map((e: any) => ({
      template: trimStr(e.template),
      output: trimStr(e.output) || '.env',
      mode: trimStr(e.mode) || '0600',
      on_missing: trimStr(e.on_missing) || 'fail',
      backup_keep: typeof e.backup_keep === 'number' && Number.isFinite(e.backup_keep)
        ? e.backup_keep
        : 3,
    }))
}

/** Drop blank rows; return undefined when nothing to send. */
export function serializeConfigRender(raw: unknown): ConfigRenderRow[] | undefined {
  const rows = normalizeConfigRender(raw).filter((r) => r.template)
  if (rows.length === 0) return undefined
  return rows.map((r) => ({
    template: r.template,
    output: r.output || '.env',
    mode: r.mode || '0600',
    on_missing: r.on_missing === 'warn' ? 'warn' : 'fail',
    backup_keep: r.backup_keep,
  }))
}

export function stepAllowsConfigRender(engine: string | undefined | null): boolean {
  const u = (engine || 'shell').trim() || 'shell'
  return u === 'shell' || u === 'local' || u === 'ssh'
}

export function stepAllowsConfigDeliver(engine: string | undefined | null): boolean {
  const u = (engine || 'shell').trim() || 'shell'
  return u === 'shell' || u === 'local' || u === 'ssh' || u === 'docker'
}

export function detectConfigMode(
  step: {
    config_source?: string | null
    config_key_path?: string | null
    config_dest?: string | null
    config_render?: unknown
  },
  allowRender = true,
): ConfigFormMode {
  const source = trimStr(step.config_source)
  const key = trimStr(step.config_key_path)
  const dest = trimStr(step.config_dest)
  // Count rows (even blank) so the mode select stays on Render while editing.
  const renderRows = allowRender ? normalizeConfigRender(step.config_render) : []
  const hasRenderRows = renderRows.length > 0
  const hasDeliver = !!(source || key)

  if (!hasDeliver && !hasRenderRows && !dest) return 'none'
  if (hasDeliver && hasRenderRows) return 'sugar'
  if (hasDeliver) return 'deliver'
  if (hasRenderRows || (dest && !hasDeliver)) return allowRender ? 'render' : 'deliver'
  return 'none'
}

/** Apply mode switch; mutates step fields in place. */
export function applyConfigMode(
  step: Record<string, any>,
  mode: ConfigFormMode,
  allowRender: boolean,
): void {
  if (mode === 'none') {
    step.config_source = ''
    step.config_key_path = ''
    step.config_dest = ''
    step.config_render = []
    return
  }
  if (mode === 'deliver') {
    step.config_render = []
    return
  }
  if (!allowRender) {
    step.config_render = []
    return
  }
  if (mode === 'render') {
    step.config_source = ''
    step.config_key_path = ''
    if (!Array.isArray(step.config_render) || step.config_render.length === 0) {
      step.config_render = [emptyRenderRow()]
    } else {
      step.config_render = normalizeConfigRender(step.config_render)
      if (step.config_render.length === 0) step.config_render = [emptyRenderRow()]
    }
    return
  }
  // sugar
  if (!Array.isArray(step.config_render) || step.config_render.length === 0) {
    step.config_render = [emptyRenderRow()]
  } else {
    step.config_render = normalizeConfigRender(step.config_render)
    if (step.config_render.length === 0) step.config_render = [emptyRenderRow()]
  }
}

/** Fields for create/update API from a form step. */
export function serializeConfigForApi(step: {
  config_source?: string | null
  config_key_path?: string | null
  config_dest?: string | null
  config_render?: unknown
}): {
  config_source?: string
  config_key_path?: string
  config_dest?: string
  config_render?: ConfigRenderRow[]
} {
  const config_source = trimStr(step.config_source) || undefined
  const config_key_path = trimStr(step.config_key_path) || undefined
  const config_dest = trimStr(step.config_dest) || undefined
  const config_render = serializeConfigRender(step.config_render)
  return { config_source, config_key_path, config_dest, config_render }
}

/**
 * Validate config fields for one step. Returns an English error message or null.
 * Mirrors shared pipeline_yaml rules for deliver / render-only / sugar.
 */
export function validateStepConfig(step: {
  name?: string
  engine?: string | null
  config_source?: string | null
  config_key_path?: string | null
  config_dest?: string | null
  config_render?: unknown
}): string | null {
  const engine = (step.engine || 'shell').trim() || 'shell'
  const source = trimStr(step.config_source)
  const key = trimStr(step.config_key_path)
  const dest = trimStr(step.config_dest)
  const renders = normalizeConfigRender(step.config_render).filter((r) => r.template)
  const hasDeliver = !!(source || key)
  const hasRender = renders.length > 0

  if (!hasDeliver && !hasRender && !dest) return null

  if (!stepAllowsConfigDeliver(engine) && (hasDeliver || hasRender || dest)) {
    return 'config is only valid for shell, local, ssh, or docker steps'
  }

  if (hasRender && !stepAllowsConfigRender(engine)) {
    return "config.render is only valid for shell, local, or ssh steps"
  }

  if (!!source !== !!key) {
    return 'config.source and config.key_path must be set together (OSS deliver)'
  }

  if (hasDeliver && !dest) {
    return 'config.dest is required for OSS deliver'
  }

  if (hasRender && !dest) {
    return "config.dest is required when config.render is set"
  }

  if (!hasDeliver && !hasRender && dest) {
    return 'config.dest alone is not enough — add config.render after fetching with buz/config-fetch'
  }

  for (let i = 0; i < renders.length; i++) {
    const r = renders[i]
    if (!r.template) {
      return `config.render[${i}].template is required`
    }
    if (r.on_missing && r.on_missing !== 'fail' && r.on_missing !== 'warn') {
      return `config.render[${i}].on_missing must be 'fail' or 'warn'`
    }
  }

  return null
}

/** Short labels for Definition / read-only display. */
export function formatConfigRenderSummary(raw: unknown): string {
  const rows = normalizeConfigRender(raw).filter((r) => r.template)
  if (rows.length === 0) return ''
  return rows.map((r) => `${r.template} → ${r.output || '.env'}`).join(', ')
}
