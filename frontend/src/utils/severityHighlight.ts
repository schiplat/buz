/**
 * Split diagnostic text so severity words and problem tokens (unknown keys,
 * quoted field names, path segments) can be highlighted in the float panel.
 */

export type SeveritySegment = { text: string; kind: string | null }

/** Whole-word severity / outcome tokens to highlight (case-insensitive). */
const SEVERITY_RE =
  /\b(errors?|warnings?|failed|fail(?:ure|ing)?|invalid|rejected|missing|required|denied|timeout|timed\s+out)\b/gi

/** Backtick / quote wrapped tokens — usually the bad key or expected name. */
const QUOTED_RE = /[`'"]([A-Za-z0-9_.\[\]-]+)[`'"]/g

/** Path-like tokens such as steps[0] / steps[0].run / concurrency.mode */
const PATH_RE = /\b(steps\[\d+\](?:\.[A-Za-z0-9_-]+)*|[A-Za-z_][\w-]*(?:\.[A-Za-z_][\w-]*)+)(?=\W|$)/g

export function splitSeveritySegments(text: string): SeveritySegment[] {
  return splitDiagnosticSegments(text)
}

/** Full diagnostic highlighter: quoted problem keys + severity words + paths. */
export function splitDiagnosticSegments(text: string): SeveritySegment[] {
  if (!text) return []
  // Collect non-overlapping matches with priority: quoted > path > severity
  type Hit = { start: number; end: number; kind: string; text: string }
  const hits: Hit[] = []

  const pushAll = (re: RegExp, kindFor: (m: RegExpExecArray) => string | null) => {
    const r = new RegExp(re.source, re.flags)
    let m: RegExpExecArray | null
    while ((m = r.exec(text)) !== null) {
      const kind = kindFor(m)
      if (!kind) continue
      hits.push({ start: m.index, end: m.index + m[0].length, kind, text: m[0] })
    }
  }

  pushAll(QUOTED_RE, () => 'problem')
  pushAll(PATH_RE, (m) => {
    // Skip if already covered by a quoted hit
    const start = m.index
    if (hits.some((h) => start >= h.start && start < h.end)) return null
    // Avoid marking plain severity words as paths
    if (/^(errors?|warnings?|failed|fail(?:ure|ing)?|invalid|rejected|missing|required|denied|timeout)$/i.test(m[0])) {
      return null
    }
    return 'path'
  })
  pushAll(SEVERITY_RE, (m) => {
    const start = m.index
    if (hits.some((h) => start >= h.start && start < h.end)) return null
    return normalizeSeverityKind(m[0])
  })

  hits.sort((a, b) => a.start - b.start || b.end - a.end)
  const merged: Hit[] = []
  for (const h of hits) {
    const last = merged[merged.length - 1]
    if (last && h.start < last.end) continue
    merged.push(h)
  }

  if (!merged.length) return [{ text, kind: null }]

  const out: SeveritySegment[] = []
  let cursor = 0
  for (const h of merged) {
    if (h.start > cursor) out.push({ text: text.slice(cursor, h.start), kind: null })
    out.push({ text: h.text, kind: h.kind })
    cursor = h.end
  }
  if (cursor < text.length) out.push({ text: text.slice(cursor), kind: null })
  return out
}

function normalizeSeverityKind(raw: string): string {
  const t = raw.toLowerCase().replace(/\s+/g, ' ')
  if (t === 'errors' || t === 'error') return 'error'
  if (t === 'warnings' || t === 'warning') return 'warning'
  if (t === 'failed' || t === 'fail' || t === 'failure' || t === 'failing') return 'failed'
  if (t === 'timed out' || t === 'timeout') return 'timeout'
  if (t === 'invalid' || t === 'rejected' || t === 'denied') return 'invalid'
  if (t === 'missing' || t === 'required') return 'required'
  return t
}

/** Tailwind classes for an inline severity / problem token. */
export function severityWordClass(kind: string | null): string {
  switch (kind) {
    case 'problem':
      return 'font-mono font-semibold text-destructive bg-destructive/15 rounded px-1 py-px'
    case 'path':
      return 'font-mono font-medium text-destructive/90 bg-destructive/10 rounded px-1 py-px'
    case 'error':
    case 'failed':
    case 'invalid':
      return 'font-semibold text-destructive'
    case 'warning':
    case 'timeout':
      return 'font-semibold text-amber-600 dark:text-amber-400'
    case 'required':
      return 'font-semibold text-orange-600 dark:text-orange-400'
    default:
      return 'text-muted-foreground'
  }
}

/** Compact row badge classes (ERROR / WARNING). */
export function severityBadgeClass(level: 'error' | 'warning'): string {
  if (level === 'error') {
    return 'bg-destructive/15 text-destructive ring-destructive/25'
  }
  return 'bg-amber-500/15 text-amber-700 dark:text-amber-400 ring-amber-500/25'
}
