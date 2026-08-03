import type { YamlParsedData, YamlStepData } from '@/api/pipelines'

/** Parse YAML duration (`30s` / `5m` / `1h`) to seconds; 0 if empty/invalid. */
export function parseDurationToSeconds(raw?: string | null): number {
  if (!raw) return 0
  const m = String(raw).trim().match(/^(\d+)\s*([smhSMH]?)$/)
  if (!m) return 0
  const n = parseInt(m[1], 10)
  const u = (m[2] || 's').toLowerCase()
  if (u === 'm') return n * 60
  if (u === 'h') return n * 3600
  return n
}

/** Map validated YAML steps into the Definition step card shape. */
export function yamlStepsToDefTasks(steps: YamlStepData[]) {
  return steps.map((s) => ({
    name: s.name || 'unnamed',
    exec_type: s.uses || 'shell',
    command_template: s.run || '',
    image: s.image || '',
    volumes: s.volumes
      ? String(s.volumes)
          .split(',')
          .map((x) => x.trim())
          .filter(Boolean)
      : [],
    host_names: (s.host || '').toString(),
    host_id: '',
    host_labels: s.host_labels || '',
    work_dir: s.dir || '',
    timeout_seconds: parseDurationToSeconds(s.timeout),
    retry_count: s.retry || 0,
    fail_strategy: s.continue_on_error ? 'continue' : 'fail',
    privileged: !!s.privileged,
    registry_secret: s.registry_secret || '',
    config_source: s.config_source || '',
    config_key_path: s.config_key_path || '',
    config_dest: s.config_dest || '',
    config_render: s.config_render || null,
    step_secrets: Array.isArray(s.secrets) ? s.secrets.join(', ') : '',
    step_env: s.env && typeof s.env === 'object' ? s.env : null,
  }))
}

export type DefinitionConcurrency = {
  configured: boolean
  group: string | null
  mode: string | null
  maxQueue: number | null
  queueTimeoutSec: number | null
}

export function concurrencyFromYaml(parsed: YamlParsedData): DefinitionConcurrency {
  if (parsed.concurrency_config?.group) {
    const cc = parsed.concurrency_config
    return {
      configured: true,
      group: cc.group,
      mode: (cc.mode || '').trim() || 'discard-old',
      maxQueue: cc.max_queue ?? 3,
      queueTimeoutSec: parseDurationToSeconds(cc.queue_timeout) || 1800,
    }
  }
  if (parsed.concurrency && String(parsed.concurrency).trim()) {
    return {
      configured: true,
      group: String(parsed.concurrency).trim(),
      mode: 'discard-old',
      maxQueue: 3,
      queueTimeoutSec: 1800,
    }
  }
  return {
    configured: false,
    group: null,
    mode: null,
    maxQueue: null,
    queueTimeoutSec: null,
  }
}

/** Collect declared + referenced secret names from validated YAML. */
export function collectSecretNamesFromYaml(parsed: YamlParsedData): string[] {
  const secretNames = new Set<string>(parsed.secrets || [])
  for (const s of parsed.steps || []) {
    for (const n of s.secrets || []) secretNames.add(n)
    for (const field of [s.registry_secret, s.config_source]) {
      if (!field) continue
      const m = String(field).match(/\$\{secrets\.([^}]+)\}/)
      if (m) secretNames.add(m[1])
      else if (!String(field).includes('$')) secretNames.add(String(field))
    }
  }
  for (const ch of parsed.notify || []) {
    const wh = ch.webhook
    if (!wh) continue
    const m = String(wh).match(/\$\{secrets\.([^}]+)\}/)
    if (m) secretNames.add(m[1])
    else if (!String(wh).includes('$')) secretNames.add(String(wh))
  }
  return [...secretNames]
}
