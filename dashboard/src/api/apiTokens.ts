import apiClient from './client'

export type ApiTokenScope = 'read' | 'ops' | 'full'

export interface ApiToken {
  id: string
  user_id: string
  username: string
  name: string
  prefix: string
  created_at: string
  last_used_at: string | null
  revoked: boolean
  scope: ApiTokenScope
}

export interface ApiTokenCreateResult {
  id: string
  name: string
  token: string
  prefix: string
  created_at: string
  scope: ApiTokenScope
}

export const apiTokensApi = {
  async list(): Promise<{ items: ApiToken[] }> {
    const resp = await apiClient.get('/api-tokens')
    return resp.data as { items: ApiToken[] }
  },
  async create(name: string, scope: 'read' | 'ops'): Promise<ApiTokenCreateResult> {
    const resp = await apiClient.post('/api-tokens', { name, scope })
    return resp.data as ApiTokenCreateResult
  },
  async revoke(id: string): Promise<void> {
    await apiClient.delete(`/api-tokens/${id}`)
  },
}
