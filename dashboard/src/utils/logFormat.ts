//! Job terminal log formatting (xterm).
//!
//! Every runner log line is colored through a single entry point
//! (`colorizeLogLine`) driven by three centralized tables below, so the style
//! stays consistent and is easy to extend:
//!
//!   1. `ANSI`           — the only place raw escape codes live.
//!   2. `BANNER_RULES`   — ordered classifier for `==> …` banner lines; the
//!                         `==>` marker is stripped for a cleaner display.
//!   3. `FIELD_KEYS`     — every `  key   value` field key the runner emits.
//!
//! Since runners stamp a severity on every chunk (`LogChunk.level`), error and
//! warn banners are colored straight from that level — wording changes
//! upstream can no longer misclassify them. Debug/info banners keep their
//! per-category colors via the keyword table; legacy chunks (no level) also
//! fall through to keywords.

import type { LogLevel } from '@/api/jobs'

/** Central ANSI palette. */
const ANSI = {
  reset: '\x1b[0m',
  dim: '\x1b[2m',
  boldRed: '\x1b[1;31m',
  boldYellow: '\x1b[1;33m',
  boldGreen: '\x1b[1;32m',
  boldCyan: '\x1b[1;36m',
  cyan: '\x1b[36m',
  magenta: '\x1b[35m',
  blue: '\x1b[34m',
  boldBlue: '\x1b[1;34m',
  boldWhite: '\x1b[1;37m',
} as const

/** Leading marker the runner emits on banner lines (`==> RUN`, `==> ERROR`, …). */
const BANNER_MARKER = '==>'

/**
 * Ordered banner classifier for `==> …` lines. First match wins, so specific
 * keywords (RUNNER) must precede generic ones (RUN). Each entry pairs the
 * keyword check with a palette style.
 */
const BANNER_RULES: ReadonlyArray<{ match: (upper: string) => boolean; style: string }> = [
  { match: (u) => u.includes('ERROR') || u.includes('FAILED'), style: ANSI.boldRed },
  { match: (u) => u.includes('CANCELLED') || u.includes('TIMEOUT'), style: ANSI.boldYellow },
  { match: (u) => u.includes('RUNNER'), style: ANSI.boldCyan },
  { match: (u) => u.includes('RUN'), style: ANSI.boldGreen },
  { match: (u) => u.includes('LOGIN'), style: ANSI.cyan },
  { match: (u) => u.includes('SSH'), style: ANSI.magenta },
  { match: (u) => u.includes('CONFIG'), style: ANSI.blue },
  { match: (u) => u.includes('STAGE'), style: ANSI.dim },
  { match: (u) => u.includes('RESULT'), style: ANSI.boldWhite },
]

/**
 * Every field key the runner emits as `  key   value`, both the `<16`-aligned
 * keys (e.g. `exec_type`, `config_dest`) and short keys (e.g. `note`, `cmd`).
 * Matching is longest-first so `config_dest` / `hosts_strategy` are not misread
 * as shorter keys, and `exit code` (two words) is handled correctly.
 */
const FIELD_KEYS: ReadonlyArray<string> = [
  // step echo
  'note', 'cwd', 'env', 'cmd',
  // step config
  'exec_type', 'timeout', 'fail_strategy', 'work_dir', 'image',
  'config_dest', 'config_render', 'hosts_strategy', 'hosts',
  // config prep (render-only)
  'render', 'changed', 'dest', 'mode', 'files', 'host',
  // ssh banner
  'workspace',
  // runner banner
  'version', 'os',
  // step result / failure
  'duration', 'exit code', 'command', 'hint',
]

/** Split a `  key   value` line into its dimmed key and plain value, or null. */
function splitFieldLine(line: string): { key: string; value: string } | null {
  const m = /^(\s+)(.*)$/.exec(line)
  if (!m) return null
  const afterIndent = m[2]
  let best: { key: string; value: string } | null = null
  for (const key of FIELD_KEYS) {
    if (afterIndent.startsWith(key)) {
      const value = afterIndent.slice(key.length)
      // Require >=2 spaces after the key so process output like `  cmd x` is
      // not mistaken for a metadata field line.
      if (value.startsWith('  ')) {
        if (!best || key.length > best.key.length) {
          best = { key: m[1] + key, value }
        }
      }
    }
  }
  return best
}

/** Strip the leading `==>` marker; returns the banner body or null if absent. */
function stripBannerMarker(line: string): string | null {
  const trimmed = line.trimStart()
  if (!trimmed.startsWith(BANNER_MARKER)) return null
  return trimmed.slice(BANNER_MARKER.length).replace(/^\s+/, '')
}

/** Color a `==> …` banner line: chunk level first, keyword classifier fallback. */
function colorizeBannerLine(line: string, level?: LogLevel | null): string {
  const body = stripBannerMarker(line)!
  if (level === 'error') return `${ANSI.boldRed}${body}${ANSI.reset}`
  if (level === 'warn') return `${ANSI.boldYellow}${body}${ANSI.reset}`
  const upper = body.toUpperCase()
  for (const rule of BANNER_RULES) {
    if (rule.match(upper)) {
      return `${rule.style}${body}${ANSI.reset}`
    }
  }
  // Generic banner, e.g. the SSH summary `==> <step_name>`.
  return `${ANSI.boldBlue}${body}${ANSI.reset}`
}

/** Color a single log line: banner / separator / field / continuation. */
export function colorizeLogLine(line: string, level?: LogLevel | null): string {
  const trimmed = line.trimStart()
  if (trimmed.startsWith(BANNER_MARKER)) {
    return colorizeBannerLine(line, level)
  }
  // Multi-host separator / summary lines (`--- host ---`, `--- summary ---`).
  if (trimmed.startsWith('---')) {
    return `${ANSI.dim}${line}${ANSI.reset}`
  }
  const field = splitFieldLine(line)
  if (field) {
    return `${ANSI.dim}${field.key}${ANSI.reset}${field.value}`
  }
  // Indented continuation line (e.g. a second `cmd` line) — dim whole line.
  if (/^\s+/.test(line)) {
    return `${ANSI.dim}${line}${ANSI.reset}`
  }
  return line
}

/** The slice of a log chunk the terminal formatter needs. */
export interface LogChunkLike {
  content: string
  level?: LogLevel | null
}

/** Dim clock + optional ANSI for banner / body lines. */
export function formatLogChunkForTerminal(ts: string, chunk: LogChunkLike): string {
  const dim = ANSI.dim
  const reset = ANSI.reset
  const normalized = chunk.content.replace(/\r\n/g, '\n').replace(/\r/g, '\n')
  const lines = normalized.split('\n')
  if (lines.length > 0 && lines[lines.length - 1] === '') {
    lines.pop()
  }
  if (lines.length === 0) {
    return `${dim}[${ts}]${reset}\r\n`
  }
  return (
    lines
      .map((line) => `${dim}[${ts}]${reset} ${colorizeLogLine(line, chunk.level)}`)
      .join('\r\n') + '\r\n'
  )
}
