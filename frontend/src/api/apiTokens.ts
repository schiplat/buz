import apiClient from './client'

export interface ApiToken {
  id: string
  user_id: string
  username: string
  name: string
  prefix: string
  created_at: string
  last_used_at: string | null
  revoked: boolean
}

export interface ApiTokenCreateResult {
  id: string
  name: string
  token: string
  prefix: string
  created_at: string
}

export const apiTokensApi = {
  async list(): Promise<{ items: ApiToken[] }> {
    const resp = await apiClient.get('/api-tokens')
    return resp.data as { items: ApiToken[] }
  },
  async create(name: string): Promise<ApiTokenCreateResult> {
    const resp = await apiClient.post('/api-tokens', { name })
    return resp.data as ApiTokenCreateResult
  },
  async revoke(id: string): Promise<void> {
    await apiClient.delete(`/api-tokens/${id}`)
  },
}
