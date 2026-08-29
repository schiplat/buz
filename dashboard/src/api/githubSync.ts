import apiClient from './client'

/** Push state of a secret/variable → repo binding (metadata only — never values). */
export type GitHubPushStatus = 'pending' | 'ok' | 'failed'

/** Most recent operation performed on the binding. */
export type GitHubBindOp = 'bind' | 'push' | 'unbind'

export interface GitHubBind {
  id: string
  repo_id: string
  connection_name: string
  owner: string
  repo: string
  last_pushed_at: string | null
  last_status: GitHubPushStatus
  last_error: string
  /** Last operation: 'bind' (added), 'push' (value pushed), 'unbind' (remote cleanup failed). */
  last_op: GitHubBindOp
}

export interface GitHubConnection {
  id: string
  name: string
  api_base_url: string
  /** Whether a token is stored (never returned). */
  has_token: boolean
  /** Comma-separated owners to auto-fetch; empty string disables auto-fetch. */
  fetch_owners: string
  /** Auto-fetch interval in minutes; null disables auto-fetch. */
  fetch_interval_minutes: number | null
  /** Last successful fetch (manual or auto) timestamp, or null. */
  last_fetch_at: string | null
  updated_at: string | null
}

export interface GitHubRepo {
  id: string
  connection_id: string
  connection_name: string
  owner: string
  repo: string
  /** Web URL to the repo (derived from the connection's API base). */
  url: string
  updated_at: string | null
  /** Secrets / variables bound to this repo (metadata only — never values). */
  binds: GitHubRepoBind[]
}

/** One secret/var bound to a repo, as seen from the repo side. */
export interface GitHubRepoBind {
  kind: 'secret' | 'variable'
  id: string
  name: string
  last_status: GitHubPushStatus
  last_op: GitHubBindOp
}

export interface GitHubPushResult {
  bind_id: string
  owner?: string
  repo?: string
  status: 'ok' | 'failed'
  error?: string
}

export interface FetchReposResult {
  added: number
  existing: number
}

export const githubSyncApi = {
  // ── connections ──
  async listConnections(): Promise<GitHubConnection[]> {
    const resp = await apiClient.get('/github-sync/connections')
    return resp.data as GitHubConnection[]
  },
  async createConnection(data: {
    name: string
    api_base_url?: string
    token: string
    fetch_owners?: string
    fetch_interval_minutes?: number | null
  }): Promise<GitHubConnection> {
    const resp = await apiClient.post('/github-sync/connections', data)
    return resp.data as GitHubConnection
  },
  async updateConnection(
    id: string,
    data: {
      name?: string
      api_base_url?: string
      token?: string
      fetch_owners?: string | null
      fetch_interval_minutes?: number | null
    },
  ): Promise<GitHubConnection> {
    const resp = await apiClient.put(`/github-sync/connections/${id}`, data)
    return resp.data as GitHubConnection
  },
  async deleteConnection(id: string): Promise<void> {
    await apiClient.delete(`/github-sync/connections/${id}`)
  },

  // ── repos ──
  async listRepos(): Promise<GitHubRepo[]> {
    const resp = await apiClient.get('/github-sync/repos')
    return resp.data as GitHubRepo[]
  },
  async createRepo(data: { connection_id: string; owner: string; repo: string }): Promise<GitHubRepo> {
    const resp = await apiClient.post('/github-sync/repos', data)
    return resp.data as GitHubRepo
  },
  async updateRepo(
    id: string,
    data: { owner?: string; repo?: string },
  ): Promise<GitHubRepo> {
    const resp = await apiClient.put(`/github-sync/repos/${id}`, data)
    return resp.data as GitHubRepo
  },
  async deleteRepo(id: string): Promise<void> {
    await apiClient.delete(`/github-sync/repos/${id}`)
  },
  async fetchRepos(data: { connection_id: string; owner: string }): Promise<FetchReposResult> {
    // Paginated GitHub list — can exceed the default 8s client timeout.
    const resp = await apiClient.post('/github-sync/repos/fetch', data, { timeout: 60_000 })
    return resp.data as FetchReposResult
  },
  async testConnection(data: {
    connection_id: string
    owner?: string
    repo?: string
  }): Promise<{ ok: boolean; login?: string }> {
    const resp = await apiClient.post('/github-sync/test', data, { timeout: 30_000 })
    return resp.data as { ok: boolean; login?: string }
  },

  // ── secret binds ──
  async listSecretBinds(secretId: string): Promise<GitHubBind[]> {
    const resp = await apiClient.get(`/secrets/${secretId}/github-binds`)
    return resp.data as GitHubBind[]
  },
  async addSecretBind(secretId: string, repoId: string): Promise<GitHubBind> {
    const resp = await apiClient.post(`/secrets/${secretId}/github-binds`, { repo_id: repoId })
    return resp.data as GitHubBind
  },
  async deleteSecretBind(secretId: string, bindId: string): Promise<void> {
    await apiClient.delete(`/secrets/${secretId}/github-binds/${bindId}`)
  },
  async pushSecretBinds(
    secretId: string,
    opts?: { bind_id?: string },
  ): Promise<{ results: GitHubPushResult[] }> {
    // Single-bind push talks to GitHub (public key + put); 30s covers slow links.
    // Push All should call this once per bind — do not rely on bulk push with the
    // global 8s axios timeout.
    const resp = await apiClient.post(`/secrets/${secretId}/github-binds/push`, null, {
      params: opts?.bind_id ? { bind_id: opts.bind_id } : undefined,
      timeout: 30_000,
    })
    return resp.data as { results: GitHubPushResult[] }
  },

  // ── variable binds ──
  async listVariableBinds(varId: string): Promise<GitHubBind[]> {
    const resp = await apiClient.get(`/variables/${varId}/github-binds`)
    return resp.data as GitHubBind[]
  },
  async addVariableBind(varId: string, repoId: string): Promise<GitHubBind> {
    const resp = await apiClient.post(`/variables/${varId}/github-binds`, { repo_id: repoId })
    return resp.data as GitHubBind
  },
  async deleteVariableBind(varId: string, bindId: string): Promise<void> {
    await apiClient.delete(`/variables/${varId}/github-binds/${bindId}`)
  },
  async pushVariableBinds(
    varId: string,
    opts?: { bind_id?: string },
  ): Promise<{ results: GitHubPushResult[] }> {
    const resp = await apiClient.post(`/variables/${varId}/github-binds/push`, null, {
      params: opts?.bind_id ? { bind_id: opts.bind_id } : undefined,
      timeout: 30_000,
    })
    return resp.data as { results: GitHubPushResult[] }
  },
}
