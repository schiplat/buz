import { beforeEach, describe, expect, it, vi } from 'vitest'

const { mockClient } = vi.hoisted(() => ({
  mockClient: {
    get: vi.fn(),
    post: vi.fn(),
    put: vi.fn(),
    patch: vi.fn(),
    delete: vi.fn(),
  },
}))

vi.mock('@/api/client', () => ({ default: mockClient }))

import { sessionPolicyApi } from '@/api/sessionPolicy'

function ok(data: unknown) {
  return { data }
}

beforeEach(() => {
  vi.clearAllMocks()
})

describe('sessionPolicyApi', () => {
  it('reads the effective policy', async () => {
    mockClient.get.mockResolvedValue(
      ok({
        ttl_hours: 24,
        ttl_default_hours: 24,
        ttl_overridden: false,
        idle_minutes: 0,
        idle_default_minutes: 0,
        idle_overridden: false,
      }),
    )
    const policy = await sessionPolicyApi.get()
    expect(mockClient.get).toHaveBeenCalledWith('/auth/session-policy')
    expect(policy.ttl_hours).toBe(24)
  })

  it('saves values and clears overrides with null', async () => {
    mockClient.put.mockResolvedValue(
      ok({
        ttl_hours: 8,
        ttl_default_hours: 24,
        ttl_overridden: true,
        idle_minutes: 15,
        idle_default_minutes: 0,
        idle_overridden: true,
      }),
    )
    await sessionPolicyApi.update({ ttl_hours: 8, idle_minutes: 15 })
    expect(mockClient.put).toHaveBeenCalledWith('/auth/session-policy', {
      ttl_hours: 8,
      idle_minutes: 15,
    })

    await sessionPolicyApi.update({ ttl_hours: null, idle_minutes: null })
    expect(mockClient.put).toHaveBeenLastCalledWith('/auth/session-policy', {
      ttl_hours: null,
      idle_minutes: null,
    })
  })
})
