/**
 * CodeMirror completion source for Buz pipeline YAML.
 * Keywords + optional secret/variable names from Manager.
 */

import type { Completion, CompletionContext, CompletionResult } from '@codemirror/autocomplete'

const TOP_LEVEL: Completion[] = [
  { label: 'name', type: 'property', info: 'Pipeline name (required)' },
  { label: 'description', type: 'property', info: 'Optional description' },
  { label: 'team', type: 'property', info: 'Team slug (default: default)' },
  { label: 'environments', type: 'property', info: 'Required target environment slug allowlist; every trigger must choose one' },
  { label: 'owner_group', type: 'property', info: 'Alias of team' },
  { label: 'on', type: 'property', info: 'Trigger: manual / webhook / cron' },
  { label: 'runner', type: 'property', info: 'labels / timeout' },
  { label: 'concurrency', type: 'property', info: 'group / mode / max_queue' },
  { label: 'env', type: 'property', info: 'Pipeline-level env map' },
  { label: 'variables', type: 'property', info: 'Platform variable name allowlist' },
  { label: 'secrets', type: 'property', info: 'Secret name allowlist (no auto-inject)' },
  { label: 'steps', type: 'property', info: 'Ordered step list' },
]

const TRIGGER: Completion[] = [
  { label: 'manual', type: 'keyword', info: 'Manual trigger (+ inputs)' },
  { label: 'webhook', type: 'keyword', info: 'Webhook path trigger' },
  { label: 'cron', type: 'keyword', info: 'Cron schedule trigger' },
  { label: 'inputs', type: 'property' },
  { label: 'path', type: 'property', info: 'Webhook path segment' },
  { label: 'schedule', type: 'property', info: 'Cron expression' },
  { label: 'required', type: 'property' },
  { label: 'default', type: 'property' },
  { label: 'masking', type: 'property', info: 'Redact input in logs/DB' },
]

const RUNNER: Completion[] = [
  { label: 'labels', type: 'property', info: 'Runner must match ALL labels' },
  { label: 'timeout', type: 'property', info: 'e.g. 60m' },
]

const CONCURRENCY: Completion[] = [
  { label: 'group', type: 'property' },
  { label: 'mode', type: 'property', info: 'discard-old | queue | discard-new' },
  { label: 'max_queue', type: 'property' },
  { label: 'queue_timeout', type: 'property' },
  { label: 'discard-old', type: 'keyword' },
  { label: 'discard-new', type: 'keyword' },
  { label: 'queue', type: 'keyword' },
]

const STEP: Completion[] = [
  { label: 'name', type: 'property' },
  {
    label: 'run',
    type: 'property',
    info: 'Shell script / command; on a uses: step it is appended after the template run block',
  },
  { label: 'engine', type: 'property', info: 'shell | docker | ssh (on uses: steps: overrides the template engine within its declared engines)' },
  {
    label: 'uses',
    type: 'property',
    info: 'Pinned step template ref: <namespace>/<name>@<pin> (local index). Inputs go under with:; image stays template-owned; engine: overrides within the template engines; run: appends; capability requests come only from the plugin.',
  },
  {
    label: 'with',
    type: 'property',
    info: 'inputs for a uses: step → INPUT_*; values may use ${secrets.X} / ${vars.X} / ${env.X}',
  },
  { label: 'image', type: 'property', info: 'Docker image (engine: docker)' },
  { label: 'volumes', type: 'property', info: 'host:container binds' },
  { label: 'host', type: 'property', info: 'SSH host name(s)' },
  { label: 'host_labels', type: 'property', info: 'SSH host label union' },
  {
    label: 'hosts_strategy',
    type: 'property',
    info: 'SSH multi-host fan-out: chain (default) | parallel',
  },
  {
    label: 'dir',
    type: 'property',
    info: 'Working directory (alias of work_dir); on a uses: step it overrides the template default_dir',
  },
  {
    label: 'work_dir',
    type: 'property',
    info: 'Step working directory, workspace-relative; on a uses: step it overrides the template default_dir',
  },
  { label: 'timeout', type: 'property' },
  { label: 'retry', type: 'property' },
  { label: 'continue-on-error', type: 'property' },
  { label: 'continue_on_error', type: 'property' },
  { label: 'privileged', type: 'property', info: 'Docker privileged / DinD' },
  { label: 'secrets', type: 'property', info: 'Step secret allowlist' },
  { label: 'env', type: 'property', info: 'Map KEY: ${secrets.NAME}' },
  { label: 'config', type: 'property', info: 'Render-only config block (dest + render); fetch with buz/config-fetch' },
  { label: 'dest', type: 'property', info: '→ $BUZ_CONFIG_DIR' },
  { label: 'render', type: 'property' },
  { label: 'template', type: 'property' },
  { label: 'output', type: 'property' },
  { label: 'mode', type: 'property' },
  { label: 'on_missing', type: 'property', info: 'fail | warn' },
  { label: 'backup_keep', type: 'property', info: 'timestamped .env backups to keep (default 3, 0 disables)' },
]

const ENGINE_VALUES: Completion[] = [
  { label: 'shell', type: 'keyword', apply: 'shell' },
  { label: 'docker', type: 'keyword', apply: 'docker' },
  { label: 'ssh', type: 'keyword', apply: 'ssh' },
]


export type PipelineYamlCompletionOptions = {
  secretNames?: string[]
  variableNames?: string[]
  environmentSlugs?: string[]
  /** `id@version` for every plugin available to this Manager. Empty/absent
   *  means "no suggestions" — a hardcoded list would drift from the real
   *  plugin library and suggest references that cannot resolve. */
  pluginRefs?: Array<{ uses: string; reviewed?: boolean; description?: string }>
}

function lineBefore(ctx: CompletionContext): string {
  const line = ctx.state.doc.lineAt(ctx.pos)
  return line.text.slice(0, ctx.pos - line.from)
}

function indentOf(line: string): number {
  const m = line.match(/^(\s*)/)
  return m ? m[1].length : 0
}

/** Best-effort section from surrounding lines (indent-aware). */
function guessSection(ctx: CompletionContext): string {
  const doc = ctx.state.doc
  const cur = doc.lineAt(ctx.pos)
  const curIndent = indentOf(cur.text)
  for (let n = cur.number; n >= 1; n--) {
    const line = doc.line(n)
    const t = line.text.trim()
    if (!t || t.startsWith('#')) continue
    const ind = indentOf(line.text)
    if (ind > curIndent) continue
    if (/^on\s*:/.test(t)) return 'on'
    if (/^runner\s*:/.test(t)) return 'runner'
    if (/^concurrency\s*:/.test(t)) return 'concurrency'
    if (/^steps\s*:/.test(t)) return 'steps'
    if (/^env\s*:/.test(t) && ind === 0) return 'env'
    if (/^secrets\s*:/.test(t) && ind === 0) return 'secrets'
    if (/^environments\s*:/.test(t) && ind === 0) return 'environments'
    if (/^variables\s*:/.test(t) && ind === 0) return 'variables'
    if (ind === 0 && /^[a-z_]+\s*:/.test(t)) return 'top'
  }
  return 'top'
}

function uniqueByLabel(items: Completion[]): Completion[] {
  const seen = new Set<string>()
  const out: Completion[] = []
  for (const c of items) {
    if (seen.has(c.label)) continue
    seen.add(c.label)
    out.push(c)
  }
  return out
}

export function createPipelineYamlCompletionSource(opts: PipelineYamlCompletionOptions = {}) {
  return (ctx: CompletionContext): CompletionResult | null => {
    const word = ctx.matchBefore(/[\w${}.:@/-]*/)
    if (!word && !ctx.explicit) return null
    if (word && word.from === word.to && !ctx.explicit) return null

    const before = lineBefore(ctx)
    const section = guessSection(ctx)
    let options: Completion[] = []
    let from = word ? word.from : ctx.pos
    const engineValue = before.match(/^\s*(?:-\s*)?engine\s*:\s*([\w-]*)$/)
    const templateValue = before.match(/^\s*(?:-\s*)?uses\s*:\s*([\w/@.:-]*)$/)
    const secretRef = before.match(/\$\{secrets\.([A-Za-z0-9_]*)$/)
    const variableRef = before.match(/\$\{vars\.([A-Za-z0-9_]*)$/)
    const envRef = before.match(/\$\{env\.([A-Za-z0-9_.]*)$/)

    if (engineValue) {
      options = ENGINE_VALUES
      from = ctx.pos - engineValue[1].length
    } else if (templateValue) {
      options = (opts.pluginRefs || []).map((plugin) => ({
        label: plugin.uses,
        type: 'keyword',
        apply: plugin.uses,
        info: [plugin.reviewed ? 'reviewed' : 'not reviewed', plugin.description]
          .filter(Boolean)
          .join(' · '),
      }))
      from = ctx.pos - templateValue[1].length
    } else if (secretRef) {
      from = ctx.pos - secretRef[1].length
      options = (opts.secretNames || []).map((name) => ({
        label: name,
        type: 'variable',
        apply: name,
        info: 'Secret name',
      }))
    } else if (variableRef) {
      from = ctx.pos - variableRef[1].length
      options = (opts.variableNames || []).map((name) => ({
        label: name,
        type: 'variable',
        apply: name,
        info: 'Platform variable',
      }))
    } else if (envRef) {
      // ${env.KEY} values resolve on the Runner from a previous step's
      // $BUZ_ENV — the Manager has no key list to complete. Permit them by
      // staying quiet instead of suggesting step fields mid-value.
      return null
    } else if (/\$\{$/.test(before) || /\$\{[a-z]*$/.test(before)) {
      from = before.endsWith('${') ? ctx.pos : ctx.pos - (before.match(/\$\{([a-z]*)$/)?.[1].length || 0)
      options = [
        { label: 'secrets.', type: 'keyword', apply: 'secrets.', info: 'Secret expression' },
        { label: 'vars.', type: 'keyword', apply: 'vars.', info: 'Variable expression' },
        { label: 'env.', type: 'keyword', apply: 'env.', info: 'Dynamic $BUZ_ENV value written by an earlier step (Runner-resolved)' },
      ]
    } else if (section === 'on') {
      options = [...TRIGGER]
    } else if (section === 'runner') {
      options = [...RUNNER]
    } else if (section === 'concurrency') {
      options = [...CONCURRENCY]
    } else if (section === 'steps') {
      options = [...STEP]
      if ((opts.secretNames || []).length) {
        options.push(
          ...opts.secretNames!.map((name) => ({
            label: name,
            type: 'variable',
            info: 'Secret (allowlist / env map)',
          })),
        )
      }
    } else if (section === 'secrets') {
      options = (opts.secretNames || []).map((name) => ({
        label: name,
        type: 'variable',
        info: 'Existing secret',
      }))
    } else if (section === 'environments') {
      options = (opts.environmentSlugs || []).map((slug) => ({
        label: slug,
        type: 'variable',
        info: 'Registered environment slug',
      }))
    } else if (section === 'variables') {
      options = (opts.variableNames || []).map((name) => ({
        label: name,
        type: 'variable',
        info: 'Existing variable',
      }))
    } else {
      options = [...TOP_LEVEL]
    }

    options = uniqueByLabel(options)
    if (!options.length) return null

    return {
      from,
      options,
      validFor: /^[\w${}.:@/-]*$/,
    }
  }
}
