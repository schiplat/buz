import { describe, expect, it, vi, beforeEach } from 'vitest'

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

import { sandboxHostsApi } from '@/api/sandboxHosts'

function ok(data: unknown) {
  return { data }
}

beforeEach(() => {
  vi.clearAllMocks()
})

describe('sandboxHostsApi', () => {
  it('lists hosts and unwraps the hosts array', async () => {
    mockClient.get.mockResolvedValue(ok({ hosts: [{ host_id: 'h', instance_id: 'i', status: 'insync' }] }))
    const hosts = await sandboxHostsApi.list()
    expect(mockClient.get).toHaveBeenCalledWith('/ops/sandbox-hosts')
    expect(hosts[0].instance_id).toBe('i')
  })

  it('enrolls and returns the one-time token', async () => {
    mockClient.post.mockResolvedValue(ok({ token: 'buzsd_x' }))
    const { token } = await sandboxHostsApi.enroll('host-a', 'sandboxd-01', 8)
    expect(mockClient.post).toHaveBeenCalledWith('/ops/sandbox-hosts', {
      host_id: 'host-a',
      instance_id: 'sandboxd-01',
      max_resource_slots: 8,
    })
    expect(token).toBe('buzsd_x')
  })

  it('sends If-Match from the desired version on publish', async () => {
    mockClient.put.mockResolvedValue(ok({ pending: false, version: 3 }))
    const doc = { profiles: {}, tiers: {}, network: 'bridge' }
    const result = await sandboxHostsApi.putConfig('h', 'i', doc, 2)
    expect(mockClient.put).toHaveBeenCalledWith('/ops/sandbox-hosts/h/i/config', doc, {
      headers: { 'If-Match': '2' },
    })
    expect(result.version).toBe(3)
    // No version → no If-Match header.
    await sandboxHostsApi.putConfig('h', 'i', doc)
    expect(mockClient.put).toHaveBeenLastCalledWith('/ops/sandbox-hosts/h/i/config', doc, {
      headers: undefined,
    })
  })

  it('rolls back, rotates tokens, and manages grants', async () => {
    mockClient.post.mockResolvedValue(ok({ version: 5 }))
    const rolled = await sandboxHostsApi.rollback('h', 'i', 1)
    expect(mockClient.post).toHaveBeenCalledWith('/ops/sandbox-hosts/h/i/rollback', { version: 1 })
    expect(rolled.version).toBe(5)

    mockClient.post.mockResolvedValue(ok({ token: 'buzsd_y' }))
    await sandboxHostsApi.rotateToken('h', 'i')
    expect(mockClient.post).toHaveBeenCalledWith('/ops/sandbox-hosts/h/i/token/rotate')

    mockClient.put.mockResolvedValue(ok({}))
    await sandboxHostsApi.grant('team-1', 'large')
    expect(mockClient.put).toHaveBeenCalledWith('/ops/resource-grants', {
      team_id: 'team-1',
      profile: 'large',
    })

    mockClient.delete.mockResolvedValue(ok(null))
    await sandboxHostsApi.revokeGrant('team-1', 'large')
    expect(mockClient.delete).toHaveBeenCalledWith('/ops/resource-grants/team-1/large')
  })

  it('approves and rejects pending changes', async () => {
    mockClient.post.mockResolvedValue(ok(null))
    await sandboxHostsApi.decideChange('c1', true, 'looks good')
    expect(mockClient.post).toHaveBeenCalledWith('/ops/sandbox-changes/c1/approve', {
      comment: 'looks good',
    })
    await sandboxHostsApi.decideChange('c1', false)
    expect(mockClient.post).toHaveBeenLastCalledWith('/ops/sandbox-changes/c1/reject', {
      comment: null,
    })
  })
})
