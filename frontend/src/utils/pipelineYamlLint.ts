/**
 * Map Manager YAML validation errors to approximate CodeMirror diagnostic positions.
 */

import type { Diagnostic } from '@codemirror/lint'
import type { YamlValidationError, YamlValidationResult } from '@/api/pipelines'

/** Find a reasonable document position for a validation field path. */
export function positionForField(doc: string, field: string): { from: number; to: number } {
  return positionForDiagnostic(doc, field, '')
}

/**
 * Resolve a highlight range for a validation error.
 * Prefers unknown-key / line:column from the message when the field is generic.
 */
export function positionForDiagnostic(
  doc: string,
  field: string,
  message: string,
): { from: number; to: number } {
  const lines = doc.split('\n')
  const f = (field || '').trim()
  const msg = message || ''

  // Prefer explicit line/column from serde ("at line 12 column 5").
  const loc = parseLineColumn(msg)
  if (loc) {
    const unknownKey = extractUnknownFieldKey(msg) || lastPathSegment(f)
    return rangeAtLine(doc, lines, loc.line - 1, unknownKey, loc.column - 1)
  }

  if (!f || f === 'yaml' || f === 'yaml.keys' || f === 'network') {
    const unknownKey = extractUnknownFieldKey(msg)
    if (unknownKey) {
      const hit = findKeyLine(lines, unknownKey)
      if (hit >= 0) return keyTokenRange(doc, lines, hit, unknownKey)
    }
    return { from: 0, to: Math.min(doc.length, lines[0]?.length ?? 0) }
  }

  // steps[0].run / steps[0].ru → Nth list item under steps
  const stepMatch = f.match(/^steps\[(\d+)\](?:\.(.+))?$/)
  if (stepMatch) {
    const idx = parseInt(stepMatch[1], 10)
    const sub = stepMatch[2]
    let stepOrdinal = -1
    let inSteps = false
    for (let i = 0; i < lines.length; i++) {
      const t = lines[i].trim()
      if (/^steps\s*:/.test(t)) {
        inSteps = true
        continue
      }
      if (inSteps && /^[a-z_][\w-]*\s*:/.test(t) && !t.startsWith('-') && indentLevel(lines[i]) === 0) {
        inSteps = false
      }
      if (inSteps && /^-\s+/.test(t)) {
        stepOrdinal++
        if (stepOrdinal === idx) {
          if (!sub) return lineRange(doc, lines, i)
          const key = sub.split('.')[0]
          for (let j = i; j < lines.length; j++) {
            if (j > i && /^-\s+/.test(lines[j].trim()) && indentLevel(lines[j]) <= indentLevel(lines[i])) {
              break
            }
            if (new RegExp(`^\\s*${escapeRe(key)}\\s*:`).test(lines[j])) {
              return keyTokenRange(doc, lines, j, key)
            }
          }
          // Unknown key may still appear as typo on a line inside the step.
          const typo = extractUnknownFieldKey(msg) || key
          for (let j = i; j < lines.length; j++) {
            if (j > i && /^-\s+/.test(lines[j].trim()) && indentLevel(lines[j]) <= indentLevel(lines[i])) {
              break
            }
            if (new RegExp(`^\\s*${escapeRe(typo)}\\s*:`).test(lines[j])) {
              return keyTokenRange(doc, lines, j, typo)
            }
          }
          return lineRange(doc, lines, i)
        }
      }
    }
  }

  // Nested path: concurrency.group → prefer last segment as YAML key
  const key = lastPathSegment(f)
  const keyRe = new RegExp(`^\\s*${escapeRe(key)}\\s*:`)
  for (let i = 0; i < lines.length; i++) {
    if (keyRe.test(lines[i])) return keyTokenRange(doc, lines, i, key)
  }

  const unknownKey = extractUnknownFieldKey(msg)
  if (unknownKey) {
    const hit = findKeyLine(lines, unknownKey)
    if (hit >= 0) return keyTokenRange(doc, lines, hit, unknownKey)
  }

  return { from: 0, to: Math.min(doc.length, lines[0]?.length ?? 0) }
}

/** Pull `ru` from `unknown field \`ru\`` / `'ru'` / `"ru"`. */
export function extractUnknownFieldKey(message: string): string | null {
  const m =
    message.match(/unknown field [`'"]([^`'"]+)[`'"]/i)
    || message.match(/unknown field:\s*[`'"]?([A-Za-z0-9_-]+)/i)
  return m?.[1] || null
}

export function parseLineColumn(message: string): { line: number; column: number } | null {
  const m = message.match(/at line\s+(\d+)\s+column\s+(\d+)/i)
  if (!m) return null
  const line = parseInt(m[1], 10)
  const column = parseInt(m[2], 10)
  if (!Number.isFinite(line) || line < 1) return null
  return { line, column: Number.isFinite(column) && column >= 1 ? column : 1 }
}

function lastPathSegment(field: string): string {
  if (!field) return field
  if (field.includes('.')) return field.split('.').pop()!
  return field
}

function findKeyLine(lines: string[], key: string): number {
  const keyRe = new RegExp(`^\\s*${escapeRe(key)}\\s*:`)
  for (let i = 0; i < lines.length; i++) {
    if (keyRe.test(lines[i])) return i
  }
  return -1
}

function indentLevel(line: string): number {
  const m = line.match(/^(\s*)/)
  return m ? m[1].length : 0
}

function escapeRe(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function lineStartOffset(lines: string[], lineIdx: number): number {
  let from = 0
  for (let i = 0; i < lineIdx; i++) from += lines[i].length + 1
  return from
}

function lineRange(doc: string, lines: string[], lineIdx: number): { from: number; to: number } {
  const from = lineStartOffset(lines, lineIdx)
  const to = from + (lines[lineIdx]?.length ?? 0)
  return { from, to: Math.min(to, doc.length) }
}

/** Highlight only the YAML key token (not the whole line / value). */
function keyTokenRange(
  doc: string,
  lines: string[],
  lineIdx: number,
  key: string,
): { from: number; to: number } {
  const line = lines[lineIdx] ?? ''
  const from0 = lineStartOffset(lines, lineIdx)
  const m = line.match(new RegExp(`^(\\s*)(${escapeRe(key)})(\\s*:)`))
  if (m) {
    const from = from0 + m[1].length
    return { from, to: Math.min(from + m[2].length, doc.length) }
  }
  return lineRange(doc, lines, lineIdx)
}

/** Highlight around a 1-based column, optionally snapping to a key token. */
function rangeAtLine(
  doc: string,
  lines: string[],
  lineIdx: number,
  key: string | null,
  columnHint: number,
): { from: number; to: number } {
  if (lineIdx < 0 || lineIdx >= lines.length) {
    return { from: 0, to: Math.min(doc.length, 1) }
  }
  if (key) {
    const token = keyTokenRange(doc, lines, lineIdx, key)
    // If key is on this line, use it; else fall through to column.
    const line = lines[lineIdx]
    if (new RegExp(`^\\s*${escapeRe(key)}\\s*:`).test(line)) return token
  }
  const from0 = lineStartOffset(lines, lineIdx)
  const col = Math.max(0, Math.min(columnHint, (lines[lineIdx]?.length ?? 0)))
  // Highlight a small span around the column (word-ish).
  const line = lines[lineIdx] ?? ''
  let start = col
  let end = col + 1
  while (start > 0 && /[A-Za-z0-9_-]/.test(line[start - 1]!)) start--
  while (end < line.length && /[A-Za-z0-9_-]/.test(line[end]!)) end++
  if (end <= start) end = Math.min(start + 1, line.length)
  return {
    from: Math.min(from0 + start, doc.length),
    to: Math.min(from0 + end, doc.length),
  }
}

export function diagnosticsFromValidation(
  doc: string,
  result: YamlValidationResult,
): Diagnostic[] {
  const out: Diagnostic[] = []
  for (const err of result.errors || []) {
    const pos = positionForDiagnostic(doc, err.field, err.message)
    out.push({
      from: pos.from,
      to: Math.max(pos.to, pos.from + 1),
      severity: 'error',
      message: formatMsg(err),
      markClass: 'cm-buz-lint-error',
    })
  }
  for (const w of result.warnings || []) {
    out.push({
      from: 0,
      to: Math.min(doc.length, doc.split('\n')[0]?.length || 1),
      severity: 'warning',
      message: w,
      markClass: 'cm-buz-lint-warning',
    })
  }
  return out
}

function formatMsg(err: YamlValidationError): string {
  if (!err.field || err.field === 'yaml' || err.field === 'yaml.keys') return err.message
  return `${err.field}: ${err.message}`
}
