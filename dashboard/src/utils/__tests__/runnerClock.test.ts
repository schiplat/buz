import { describe, expect, it } from 'vitest'
import {
  CLOCK_DANGER_SECONDS,
  CLOCK_WARN_SECONDS,
  clockSkewInfo,
  clockToneClass,
} from '../runnerClock'

describe('clockSkewInfo', () => {
  it('returns null when no measurement exists', () => {
    expect(clockSkewInfo(null)).toBeNull()
    expect(clockSkewInfo(undefined)).toBeNull()
    expect(clockSkewInfo(Number.NaN)).toBeNull()
  })

  it('treats small drift as ok', () => {
    const info = clockSkewInfo(0)
    expect(info).toEqual({ text: '+0s', tone: 'ok', title: expect.any(String) })
    expect(clockSkewInfo(CLOCK_WARN_SECONDS - 1)?.tone).toBe('ok')
    expect(clockSkewInfo(-(CLOCK_WARN_SECONDS - 1))?.tone).toBe('ok')
  })

  it('warns at the warn threshold', () => {
    expect(clockSkewInfo(CLOCK_WARN_SECONDS)?.tone).toBe('warn')
    expect(clockSkewInfo(-CLOCK_WARN_SECONDS)?.tone).toBe('warn')
    expect(clockSkewInfo(CLOCK_WARN_SECONDS)?.title).toContain('sync NTP')
  })

  it('flags danger at the Runner max skew', () => {
    expect(clockSkewInfo(CLOCK_DANGER_SECONDS)?.tone).toBe('danger')
    expect(clockSkewInfo(9600)?.tone).toBe('danger')
  })

  it('signs and words the direction', () => {
    expect(clockSkewInfo(45)?.text).toBe('+45s')
    expect(clockSkewInfo(45)?.title).toContain('45s behind')
    expect(clockSkewInfo(-45)?.text).toBe('-45s')
    expect(clockSkewInfo(-45)?.title).toContain('45s ahead')
  })

  it('rounds sub-second measurements', () => {
    expect(clockSkewInfo(12.4)?.text).toBe('+12s')
    expect(clockSkewInfo(-12.6)?.text).toBe('-13s')
  })
})

describe('clockToneClass', () => {
  it('maps tones to dashboard colors', () => {
    expect(clockToneClass('danger')).toBe('text-red-500')
    expect(clockToneClass('warn')).toBe('text-amber-500')
    expect(clockToneClass('ok')).toBe('')
  })
})
