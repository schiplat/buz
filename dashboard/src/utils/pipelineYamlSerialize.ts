/**
 * Best-effort Form/API state → YAML text for the YAML editor when Form UI is hidden.
 * Round-trip is not guaranteed for every edge case; validate before save.
 */

type EnvRow = { name: string; value: string }

type StepLike = {
  name?: string
  run?: string
  engine?: string
  image?: string
  volumes?: unknown
  secrets?: string[]
  env?: Record<string, string>
  host?: string
  host_labels?: string
  hosts_strategy?: string
  dir?: string
  timeout?: string
  retry?: number
  continue_on_error?: boolean
  privileged?: boolean
  registry_secret?: string
  config_source?: string
  config_key_path?: string
  config_dest?: string
  config_render?: Array<{
    template?: string
    output?: string
    mode?: string
    on_missing?: string
    backup_keep?: number
  }>
}

type FormLike = {
  name: string
  description: string
  /** Team slug (`team:` / `owner_group:`). Omit or `default` → default team. */
  team?: string
  trigger_type: string
  webhook_path: string
  cron_expr: string
  runner_labels: string
  runner_timeout: string
  concurrency_group: string
  concurrency_mode: string
  concurrency_max_queue: number
  concurrency_queue_timeout_seconds: number
  env: EnvRow[]
  var_ids: string[]
  secret_ids: string[]
  steps: StepLike[]
  input_defs: Array<{
    name: string
    description?: string
    required?: boolean
    default?: string
    masking?: boolean
    type?: string
    pattern?: string
    options?: string[]
  }>
}

function esc(s: string): string {
  if (/[:#{}[\],&*?|>!%@`]/.test(s) || s !== s.trim() || s === '') {
    return JSON.stringify(s)
  }
  return s
}

function indentBlock(text: string, spaces: number): string {
  const pad = ' '.repeat(spaces)
  return text
    .split('\n')
    .map((line) => (line.length ? pad + line : pad.trimEnd()))
    .join('\n')
}

function volumesToYamlList(volumes: unknown): string[] {
  if (!Array.isArray(volumes)) return []
  return volumes
    .map((v) => {
      if (typeof v === 'string') return v
      if (v && typeof v === 'object' && 'host' in v && 'container' in v) {
        const h = String((v as { host: string }).host || '').trim()
        const c = String((v as { container: string }).container || '').trim()
        return h && c ? `${h}:${c}` : ''
      }
      return ''
    })
    .filter(Boolean)
}

/** Serialize editor form state to YAML (for YAML-first edit when Form is hidden). */
export function serializePipelineFormToYaml(
  form: FormLike,
  opts?: {
    variableNames?: string[]
    secretNames?: string[]
  },
): string {
  const lines: string[] = []
  lines.push(`name: ${esc(form.name.trim() || 'unnamed')}`)
  if (form.description.trim()) {
    lines.push(`description: ${esc(form.description.trim())}`)
  }
  const teamSlug = (form.team || '').trim()
  if (teamSlug && teamSlug !== 'default') {
    lines.push(`team: ${esc(teamSlug)}`)
  }

  const tt = form.trigger_type || 'manual'
  if (tt === 'webhook' && form.webhook_path.trim()) {
    lines.push('on:')
    lines.push('  webhook:')
    lines.push(`    path: ${esc(form.webhook_path.trim())}`)
  } else if (tt === 'cron' && form.cron_expr.trim()) {
    lines.push('on:')
    lines.push('  cron:')
    lines.push(`    schedule: ${esc(form.cron_expr.trim())}`)
  } else if (form.input_defs.some((d) => d.name.trim())) {
    lines.push('on:')
    lines.push('  manual:')
    lines.push('    inputs:')
    for (const d of form.input_defs.filter((x) => x.name.trim())) {
      lines.push(`      ${d.name.trim()}:`)
      if (d.description) lines.push(`        description: ${esc(d.description)}`)
      if (d.required) lines.push('        required: true')
      if (d.default) lines.push(`        default: ${esc(d.default)}`)
      if (d.masking) lines.push('        masking: true')
      if (d.type) lines.push(`        type: ${esc(d.type)}`)
      if (d.pattern) lines.push(`        pattern: ${esc(d.pattern)}`)
      if (d.options?.length) lines.push(`        options: [${d.options.map(esc).join(', ')}]`)
    }
  }

  const labels = form.runner_labels
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean)
  if (labels.length || form.runner_timeout.trim()) {
    lines.push('runner:')
    if (labels.length) {
      lines.push(`  labels: [${labels.map(esc).join(', ')}]`)
    }
    if (form.runner_timeout.trim()) {
      lines.push(`  timeout: ${esc(form.runner_timeout.trim())}`)
    }
  }

  if (form.concurrency_group.trim()) {
    if (form.concurrency_mode) {
      lines.push('concurrency:')
      lines.push(`  group: ${esc(form.concurrency_group.trim())}`)
      lines.push(`  mode: ${esc(form.concurrency_mode)}`)
      if (form.concurrency_mode === 'queue') {
        lines.push(`  max_queue: ${form.concurrency_max_queue}`)
        lines.push(`  queue_timeout: ${form.concurrency_queue_timeout_seconds}s`)
      }
    } else {
      lines.push(`concurrency: ${esc(form.concurrency_group.trim())}`)
    }
  }

  const envRows = form.env.filter((e) => e.name.trim())
  if (envRows.length) {
    lines.push('env:')
    for (const e of envRows) {
      lines.push(`  ${e.name.trim()}: ${esc(e.value)}`)
    }
  }

  const varNames = opts?.variableNames || []
  if (varNames.length) {
    lines.push('variables:')
    for (const n of varNames) lines.push(`  - ${esc(n)}`)
  }

  const secretNames = opts?.secretNames || []
  if (secretNames.length) {
    lines.push('secrets:')
    for (const n of secretNames) lines.push(`  - ${esc(n)}`)
  }

  lines.push('steps:')
  for (const s of form.steps) {
    lines.push(`  - name: ${esc(s.name || 'unnamed')}`)
    const engine = (s.engine || 'shell').trim()
    if (engine && engine !== 'shell' && engine !== 'local') {
      lines.push(`    engine: ${esc(engine)}`)
    }
    if (s.image) lines.push(`    image: ${esc(s.image)}`)
    const vols = volumesToYamlList(s.volumes)
    if (vols.length) {
      lines.push('    volumes:')
      for (const v of vols) lines.push(`      - ${esc(v)}`)
    }
    if (s.privileged) lines.push('    privileged: true')
    if (s.registry_secret) lines.push(`    registry_secret: ${esc(s.registry_secret)}`)
    if (s.secrets?.length) {
      lines.push(`    secrets: [${s.secrets.map(esc).join(', ')}]`)
    }
    if (s.env && Object.keys(s.env).length) {
      lines.push('    env:')
      for (const [k, v] of Object.entries(s.env)) {
        lines.push(`      ${k}: ${esc(v)}`)
      }
    }
    if (s.host) {
      const hosts = s.host.split(',').map((h) => h.trim()).filter(Boolean)
      if (hosts.length === 1) lines.push(`    host: ${esc(hosts[0])}`)
      else if (hosts.length > 1) {
        lines.push('    host:')
        for (const h of hosts) lines.push(`      - ${esc(h)}`)
      }
    }
    if (s.host_labels) {
      const labs = s.host_labels.split(',').map((h) => h.trim()).filter(Boolean)
      if (labs.length === 1) lines.push(`    host_labels: ${esc(labs[0])}`)
      else if (labs.length > 1) {
        lines.push('    host_labels:')
        for (const h of labs) lines.push(`      - ${esc(h)}`)
      }
    }
    if (s.hosts_strategy) lines.push(`    hosts_strategy: ${esc(s.hosts_strategy)}`)
    if (s.dir) lines.push(`    dir: ${esc(s.dir)}`)
    if (s.timeout) lines.push(`    timeout: ${esc(s.timeout)}`)
    if (s.retry) lines.push(`    retry: ${s.retry}`)
    if (s.continue_on_error) lines.push('    continue-on-error: true')

    const hasConfig =
      !!(s.config_source || s.config_key_path || s.config_dest || (s.config_render && s.config_render.length))
    if (hasConfig) {
      lines.push('    config:')
      if (s.config_source) lines.push(`      source: ${esc(s.config_source)}`)
      if (s.config_key_path) lines.push(`      key_path: ${esc(s.config_key_path)}`)
      if (s.config_dest) lines.push(`      dest: ${esc(s.config_dest)}`)
      if (s.config_render?.length) {
        lines.push('      render:')
        for (const r of s.config_render) {
          if (!r.template) continue
          lines.push(`        - template: ${esc(r.template)}`)
          if (r.output) lines.push(`          output: ${esc(r.output)}`)
          if (r.mode) lines.push(`          mode: ${esc(r.mode)}`)
          if (r.on_missing) lines.push(`          on_missing: ${esc(r.on_missing)}`)
          if (r.backup_keep != null && r.backup_keep !== 3) {
            lines.push(`          backup_keep: ${r.backup_keep}`)
          }
        }
      }
    }

    const run = s.run || ''
    if (run.includes('\n')) {
      lines.push('    run: |')
      lines.push(indentBlock(run.replace(/\n$/, ''), 6))
    } else {
      lines.push(`    run: ${esc(run)}`)
    }
  }

  return lines.join('\n') + '\n'
}
