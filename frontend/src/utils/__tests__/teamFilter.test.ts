import { describe, expect, it } from 'vitest'
import { matchesTeamFilter } from '@/utils/teamFilter'

describe('matchesTeamFilter', () => {
  it('passes everything when All teams (empty active)', () => {
    expect(matchesTeamFilter('t1', '')).toBe(true)
    expect(matchesTeamFilter(null, '')).toBe(true)
    expect(matchesTeamFilter(undefined, '')).toBe(true)
  })

  it('matches exact team id', () => {
    expect(matchesTeamFilter('t1', 't1')).toBe(true)
    expect(matchesTeamFilter('t2', 't1')).toBe(false)
  })

  it('excludes unscoped unless includeShared', () => {
    expect(matchesTeamFilter(null, 't1')).toBe(false)
    expect(matchesTeamFilter('', 't1')).toBe(false)
    expect(matchesTeamFilter(null, 't1', { includeShared: true })).toBe(true)
    expect(matchesTeamFilter('', 't1', { includeShared: true })).toBe(true)
  })
})
