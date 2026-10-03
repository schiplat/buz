/**
 * CodeMirror completion source for Buz pipeline YAML.
 * Keywords + optional secret/variable names from Manager.
 */

import type { Completion, CompletionContext, CompletionResult } from '@codemirror/autocomplete'

const TOP_LEVEL: Completion[] = [
  { label: 'name', type: 'property', info: 'Pipeline name (required)' },
  { label: 'description', type: 'property', info: 'Optional description' },
  { label: 'team', type: 'property', info: 'Team slug (default: default)' },
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
  { label: 'run', type: 'property', info: 'Shell script / command' },
  { label: 'engine', type: 'property', info: 'shell | docker | ssh' },
  {
    label: 'uses',
    type: 'property',
    info: 'Step template ref: <namespace>/<id>@<pin> (local step index)',
  },
  { label: 'with', type: 'property', info: 'inputs for a uses: step → INPUT_*' },
  { label: 'image', type: 'property', info: 'Docker image (engine: docker)' },
  { label: 'volumes', type: 'property', info: 'host:container binds' },
  { label: 'host', type: 'property', info: 'SSH host name(s)' },
  { label: 'host_labels', type: 'property', info: 'SSH host label union' },
  {
    label: 'hosts_strategy',
    type: 'property',
    info: 'SSH multi-host fan-out: chain (default) | parallel',
  },
  { label: 'dir', type: 'property', info: 'Working directory' },
  { label: 'work_dir', type: 'property' },
  { label: 'timeout', type: 'property' },
  { label: 'retry', type: 'property' },
  { label: 'continue-on-error', type: 'property' },
  { label: 'continue_on_error', type: 'property' },
  { label: 'privileged', type: 'property', info: 'Docker privileged / DinD' },
  { label: 'secrets', type: 'property', info: 'Step secret allowlist' },
  { label: 'env', type: 'property', info: 'Map KEY: ${secrets.NAME}' },
  { label: 'registry_secret', type: 'property', info: '${secrets.NAME} for registry login' },
  { label: 'config', type: 'property', info: 'Nested config block (OSS / render)' },
  { label: 'source', type: 'property', info: 'config.source secret slot' },
  { label: 'key_path', type: 'property' },
  { label: 'dest', type: 'property', info: '→ $BUZ_CONFIG_DIR' },
  { label: 'render', type: 'property' },
  { label: 'template', type: 'property' },
  { label: 'output', type: 'property' },
  { label: 'mode', type: 'property' },
  { label: 'on_missing', type: 'property', info: 'fail | warn' },
  { label: 'backup_keep', type: 'property', info: 'timestamped .env backups to keep (default 3, 0 disables)' },
]

const USES_VALUES: Completion[] = [
  { label: 'shell', type: 'keyword', apply: 'shell' },
  { label: 'docker', type: 'keyword', apply: 'docker' },
  { label: 'ssh', type: 'keyword', apply: 'ssh' },
]

export type PipelineYamlCompletionOptions = {
  secretNames?: string[]
  variableNames?: string[]
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
    const word = ctx.matchBefore(/[\w${}.:-]*/)
    if (!word && !ctx.explicit) return null
    if (word && word.from === word.to && !ctx.explicit) return null

    const before = lineBefore(ctx)
    const section = guessSection(ctx)
    let options: Completion[] = []

    // engine: <cursor>
    if (/engine\s*:\s*[\w-]*$/.test(before.trimEnd()) || /engine\s*:\s*$/.test(before)) {
      options = USES_VALUES
    } else if (/\$\{secrets\.[A-Za-z0-9_]*$/.test(before) || /\$\{secrets\.$/.test(before)) {
      options = (opts.secretNames || []).map((name) => ({
        label: name,
        type: 'variable',
        apply: name,
        info: 'Secret name',
      }))
    } else if (/\$\{vars\.[A-Za-z0-9_]*$/.test(before) || /\$\{vars\.$/.test(before)) {
      options = (opts.variableNames || []).map((name) => ({
        label: name,
        type: 'variable',
        apply: name,
        info: 'Platform variable',
      }))
    } else if (/\$\{$/.test(before) || /\$\{[a-z]*$/.test(before)) {
      options = [
        { label: 'secrets.', type: 'keyword', apply: 'secrets.', info: 'Secret expression' },
        { label: 'vars.', type: 'keyword', apply: 'vars.', info: 'Variable expression' },
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
      from: word ? word.from : ctx.pos,
      options,
      validFor: /^[\w${}.:-]*$/,
    }
  }
}
