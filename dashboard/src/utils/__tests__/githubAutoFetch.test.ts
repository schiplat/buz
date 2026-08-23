import { describe, expect, it } from 'vitest'
import { autoFetchStatus } from '../githubAutoFetch'

describe('autoFetchStatus', () => {
  it('is off when no interval is configured', () => {
    expect(autoFetchStatus(null, '')).toEqual({ badge: null, detail: null, warn: false })
    expect(autoFetchStatus(undefined, 'schiplat')).toEqual({ badge: null, detail: null, warn: false })
  })

  it('shows the interval badge with owners when both are set', () => {
    expect(autoFetchStatus(60, 'schiplat')).toEqual({ badge: '60m', detail: 'schiplat', warn: false })
  })

  it('trims and dedups blank owner entries', () => {
    expect(autoFetchStatus(30, ' schiplat , \n fuxi , ')).toEqual({ badge: '30m', detail: 'schiplat, fuxi', warn: false })
  })

  it('truncates a long owner list with an ellipsis', () => {
    expect(autoFetchStatus(15, 'a, b, c')).toEqual({ badge: '15m', detail: 'a, b…', warn: false })
  })

  it('flags an interval without owners as needing owners', () => {
    expect(autoFetchStatus(60, '')).toEqual({
      badge: '60m',
      detail: 'no owners — add Fetch Owners to enable',
      warn: true,
    })
    expect(autoFetchStatus(60, null)).toEqual({
      badge: '60m',
      detail: 'no owners — add Fetch Owners to enable',
      warn: true,
    })
  })
})
