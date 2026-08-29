import { describe, expect, it, vi } from 'vitest'
import { pushBindsSequentially } from '@/utils/githubPushSequential'

describe('pushBindsSequentially', () => {
  const binds = [
    { id: 'b1', owner: 'org', repo: 'a' },
    { id: 'b2', owner: 'org', repo: 'b' },
    { id: 'b3', owner: 'org', repo: 'c' },
  ]

  it('pushes each bind in order and reports progress', async () => {
    const order: string[] = []
    const progress: number[] = []
    const pushOne = vi.fn(async (bind: { id: string }) => {
      order.push(bind.id)
      return { bind_id: bind.id, status: 'ok' as const }
    })

    const { failures } = await pushBindsSequentially(binds, pushOne, (p) => {
      progress.push(p.done)
    })

    expect(order).toEqual(['b1', 'b2', 'b3'])
    expect(pushOne).toHaveBeenCalledTimes(3)
    expect(failures).toEqual([])
    // Progress fires at start (done=i) and end (done=i+1) of each bind.
    expect(progress).toEqual([0, 1, 1, 2, 2, 3])
  })

  it('continues after a failed result and collects failure labels', async () => {
    const pushOne = vi.fn(async (bind: { id: string }) => {
      if (bind.id === 'b2') {
        return {
          bind_id: bind.id,
          owner: 'org',
          repo: 'b',
          status: 'failed' as const,
          error: '403 forbidden',
        }
      }
      return { bind_id: bind.id, status: 'ok' as const }
    })

    const { failures } = await pushBindsSequentially(binds, pushOne)
    expect(pushOne).toHaveBeenCalledTimes(3)
    expect(failures).toEqual(['org/b — 403 forbidden'])
  })

  it('treats thrown errors as per-bind failures without aborting the rest', async () => {
    const pushOne = vi.fn(async (bind: { id: string }) => {
      if (bind.id === 'b1') {
        throw Object.assign(new Error('timeout of 8000ms exceeded'), {
          response: { data: { message: 'timeout of 8000ms exceeded' } },
        })
      }
      return { bind_id: bind.id, status: 'ok' as const }
    })

    const { failures } = await pushBindsSequentially(binds, pushOne)
    expect(pushOne).toHaveBeenCalledTimes(3)
    expect(failures).toEqual(['org/a — timeout of 8000ms exceeded'])
  })
})
