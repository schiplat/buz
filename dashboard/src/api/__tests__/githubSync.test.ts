import { describe, expect, it, vi, beforeEach } from 'vitest'

const { mockClient } = vi.hoisted(() => ({
  mockClient: {
    get: vi.fn(),
    post: vi.fn(),
    put: vi.fn(),
    delete: vi.fn(),
  },
}))

vi.mock('@/api/client', () => ({ default: mockClient }))

import { githubSyncApi } from '@/api/githubSync'

function ok(data: unknown) {
  return { data }
}

beforeEach(() => {
  vi.clearAllMocks()
})

describe('githubSyncApi', () => {
  it('lists connections and repos', async () => {
    mockClient.get.mockResolvedValue(ok([{ id: 'c1', name: 'gh' }]))
    const conns = await githubSyncApi.listConnections()
    expect(mockClient.get).toHaveBeenCalledWith('/github-sync/connections')
    expect(conns[0].name).toBe('gh')

    mockClient.get.mockResolvedValue(ok([{ id: 'r1', owner: 'myorg' }]))
    await githubSyncApi.listRepos()
    expect(mockClient.get).toHaveBeenLastCalledWith('/github-sync/repos')
  })

  it('creates a connection with name/base/token', async () => {
    mockClient.post.mockResolvedValue(ok({ id: 'c1' }))
    await githubSyncApi.createConnection({ name: 'gh', api_base_url: 'https://api.github.com', token: 'pat' })
    expect(mockClient.post).toHaveBeenCalledWith('/github-sync/connections', {
      name: 'gh',
      api_base_url: 'https://api.github.com',
      token: 'pat',
    })
  })

  it('updates a connection without sending a token when omitted', async () => {
    mockClient.put.mockResolvedValue(ok({}))
    await githubSyncApi.updateConnection('c1', { name: 'renamed' })
    expect(mockClient.put).toHaveBeenCalledWith('/github-sync/connections/c1', { name: 'renamed' })
  })

  it('creates a connection with auto-fetch config', async () => {
    mockClient.post.mockResolvedValue(ok({ id: 'c1' }))
    await githubSyncApi.createConnection({
      name: 'gh',
      token: 'pat',
      fetch_owners: 'myorg,pagedorg',
      fetch_interval_minutes: 30,
    })
    expect(mockClient.post).toHaveBeenCalledWith('/github-sync/connections', {
      name: 'gh',
      token: 'pat',
      fetch_owners: 'myorg,pagedorg',
      fetch_interval_minutes: 30,
    })
  })

  it('updates a connection fetch config and disables via null interval', async () => {
    mockClient.put.mockResolvedValue(ok({}))
    await githubSyncApi.updateConnection('c1', { fetch_owners: 'onlyorg', fetch_interval_minutes: null })
    expect(mockClient.put).toHaveBeenCalledWith('/github-sync/connections/c1', {
      fetch_owners: 'onlyorg',
      fetch_interval_minutes: null,
    })
  })

  it('pushes secret binds with an optional single bind_id', async () => {
    mockClient.post.mockResolvedValue(ok({ results: [] }))
    await githubSyncApi.pushSecretBinds('sec-1', { bind_id: 'b1' })
    expect(mockClient.post).toHaveBeenCalledWith('/secrets/sec-1/github-binds/push', null, {
      params: { bind_id: 'b1' },
    })

    mockClient.post.mockResolvedValue(ok({ results: [] }))
    await githubSyncApi.pushSecretBinds('sec-1')
    expect(mockClient.post).toHaveBeenLastCalledWith('/secrets/sec-1/github-binds/push', null, {
      params: undefined,
    })
  })

  it('fetches repos from an owner', async () => {
    mockClient.post.mockResolvedValue(ok({ added: 3, existing: 1 }))
    const res = await githubSyncApi.fetchRepos({ connection_id: 'c1', owner: 'myorg' })
    expect(mockClient.post).toHaveBeenCalledWith('/github-sync/repos/fetch', {
      connection_id: 'c1',
      owner: 'myorg',
    })
    expect(res.added).toBe(3)
  })

  it('tests a connection', async () => {
    mockClient.post.mockResolvedValue(ok({ ok: true }))
    const res = await githubSyncApi.testConnection({ connection_id: 'c1', owner: 'myorg', repo: 'alpha' })
    expect(mockClient.post).toHaveBeenCalledWith('/github-sync/test', {
      connection_id: 'c1',
      owner: 'myorg',
      repo: 'alpha',
    })
    expect(res.ok).toBe(true)
  })

  it('manages variable binds with the variable API surface', async () => {
    mockClient.post.mockResolvedValue(ok({ id: 'bind-9' }))
    const bind = await githubSyncApi.addVariableBind('var-1', 'repo-2')
    expect(mockClient.post).toHaveBeenCalledWith('/variables/var-1/github-binds', { repo_id: 'repo-2' })
    expect(bind.id).toBe('bind-9')

    mockClient.delete.mockResolvedValue(ok(null))
    await githubSyncApi.deleteVariableBind('var-1', 'bind-9')
    expect(mockClient.delete).toHaveBeenCalledWith('/variables/var-1/github-binds/bind-9')
  })
})
