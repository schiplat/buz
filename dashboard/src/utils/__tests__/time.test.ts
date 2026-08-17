import { describe, expect, it } from 'vitest'
import { formatLocalDateTime, formatLogClock, parseUtc } from '@/utils/time'

describe('formatLogClock', () => {
  it('converts UTC to browser local HH:MM:SS', () => {
    const raw = '2026-07-28T12:41:22Z'
    const d = new Date(parseUtc(raw))
    const expected =
      `${String(d.getHours()).padStart(2, '0')}:` +
      `${String(d.getMinutes()).padStart(2, '0')}:` +
      `${String(d.getSeconds()).padStart(2, '0')}`
    expect(formatLogClock(raw)).toBe(expected)
  })

  it('treats SQLite datetime as UTC then localizes', () => {
    const raw = '2026-07-28 12:41:22'
    const d = new Date(parseUtc(raw))
    const expected =
      `${String(d.getHours()).padStart(2, '0')}:` +
      `${String(d.getMinutes()).padStart(2, '0')}:` +
      `${String(d.getSeconds()).padStart(2, '0')}`
    expect(formatLogClock(raw)).toBe(expected)
  })

  it('uses fallback when missing', () => {
    expect(formatLogClock(null, '99:99:99')).toBe('99:99:99')
    expect(formatLogClock(undefined, '--:--:--')).toBe('--:--:--')
  })
})

describe('formatLocalDateTime', () => {
  it('formats full local datetime', () => {
    const raw = '2026-07-28T12:41:22Z'
    const d = new Date(parseUtc(raw))
    const expected =
      `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')} ` +
      `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}:${String(d.getSeconds()).padStart(2, '0')}`
    expect(formatLocalDateTime(raw)).toBe(expected)
  })
})

describe('parseUtc', () => {
  it('parses SQLite UTC as epoch', () => {
    const ms = parseUtc('2026-07-28 12:41:22')
    expect(new Date(ms).toISOString()).toBe('2026-07-28T12:41:22.000Z')
  })
})
