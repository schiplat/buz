import { describe, expect, it } from 'vitest'
import {
  splitDiagnosticSegments,
  splitSeveritySegments,
  severityWordClass,
} from '../severityHighlight'

describe('splitSeveritySegments / splitDiagnosticSegments', () => {
  it('marks error / warning / failed tokens', () => {
    const segs = splitSeveritySegments('YAML syntax error: timeout failed with warning')
    const kinds = segs.filter((s) => s.kind).map((s) => ({ text: s.text, kind: s.kind }))
    expect(kinds).toEqual([
      { text: 'error', kind: 'error' },
      { text: 'timeout', kind: 'timeout' },
      { text: 'failed', kind: 'failed' },
      { text: 'warning', kind: 'warning' },
    ])
  })

  it('highlights quoted unknown field as problem', () => {
    const segs = splitDiagnosticSegments(
      "YAML error: steps[0]: unknown field `ru`, expected one of `name`, `run`",
    )
    const problems = segs.filter((s) => s.kind === 'problem').map((s) => s.text)
    expect(problems).toContain('`ru`')
    expect(problems).toContain('`name`')
    expect(problems).toContain('`run`')
    expect(segs.some((s) => s.kind === 'path' && s.text.includes('steps[0]'))).toBe(true)
  })

  it('returns plain text when no tokens', () => {
    expect(splitSeveritySegments('all good here')).toEqual([
      { text: 'all good here', kind: null },
    ])
  })

  it('maps severityWordClass for known kinds', () => {
    expect(severityWordClass('error')).toContain('destructive')
    expect(severityWordClass('warning')).toContain('amber')
    expect(severityWordClass('problem')).toContain('bg-destructive')
    expect(severityWordClass(null)).toContain('muted')
  })
})
