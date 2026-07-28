import { describe, expect, it } from 'vitest'
import { formatLogClock, parseUtc } from '@/utils/time'

describe('formatLogClock', () => {
  it('parses SQLite datetime', () => {
    expect(formatLogClock('2026-07-28 12:41:22')).toBe('12:41:22')
  })

  it('parses RFC3339', () => {
    expect(formatLogClock('2026-07-28T12:41:22Z')).toBe('12:41:22')
  })

  it('uses fallback when missing', () => {
    expect(formatLogClock(null, '99:99:99')).toBe('99:99:99')
    expect(formatLogClock(undefined, '--:--:--')).toBe('--:--:--')
  })
})

describe('parseUtc', () => {
  it('parses SQLite UTC as epoch', () => {
    const ms = parseUtc('2026-07-28 12:41:22')
    expect(new Date(ms).toISOString()).toBe('2026-07-28T12:41:22.000Z')
  })
})
