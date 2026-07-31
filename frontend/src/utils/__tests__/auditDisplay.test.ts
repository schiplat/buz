import { describe, expect, it } from 'vitest'
import { formatAuditJson, toSqliteDateBound } from '../auditDisplay'

describe('toSqliteDateBound', () => {
  it('returns undefined for empty', () => {
    expect(toSqliteDateBound('')).toBeUndefined()
    expect(toSqliteDateBound('   ')).toBeUndefined()
  })

  it('replaces T from datetime-local', () => {
    expect(toSqliteDateBound('2026-07-30T14:05')).toBe('2026-07-30 14:05')
  })

  it('passes through space-separated timestamps', () => {
    expect(toSqliteDateBound('2026-07-30 14:05:00')).toBe('2026-07-30 14:05:00')
  })
})

describe('formatAuditJson', () => {
  it('formats objects', () => {
    expect(formatAuditJson({ a: 1 })).toContain('"a": 1')
  })

  it('uses em dash for null', () => {
    expect(formatAuditJson(null)).toBe('—')
  })
})
