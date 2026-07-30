import apiClient from './client'

export interface Secret {
  id: string
  name: string
  /** Partially redacted value for display (never full plaintext). */
  value: string
  /** True when stored ciphertext cannot be decrypted (legacy plaintext) — re-save required. */
  reencrypt_required?: boolean
  owner_group: string
  team_id: string
  updated_at: string
}

export const secretsApi = {
  async list(): Promise<Secret[]> {
    const resp = await apiClient.get('/secrets')
    return resp.data as Secret[]
  },

  async create(data: {
    name: string
    value: string
    team_id: string
    owner_group?: string
  }): Promise<{ id: string; name: string; team_id: string; owner_group: string }> {
    const resp = await apiClient.post('/secrets', data)
    return resp.data
  },

  async get(id: string): Promise<Secret> {
    const resp = await apiClient.get(`/secrets/${id}`)
    return resp.data as Secret
  },

  async update(
    id: string,
    data: { value: string; team_id?: string; owner_group?: string },
  ): Promise<void> {
    await apiClient.put(`/secrets/${id}`, data)
  },

  async delete(id: string): Promise<void> {
    await apiClient.delete(`/secrets/${id}`)
  },
}
