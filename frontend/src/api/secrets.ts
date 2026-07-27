import apiClient from './client'

export interface Secret {
  id: string
  name: string
  value: string
  updated_at: string
}

export const secretsApi = {
  async list(): Promise<Secret[]> {
    const resp = await apiClient.get('/secrets')
    return resp.data as Secret[]
  },

  async create(data: { name: string; value: string }): Promise<{ id: string; name: string }> {
    const resp = await apiClient.post('/secrets', data)
    return resp.data as { id: string; name: string }
  },

  async get(id: string): Promise<Secret> {
    const resp = await apiClient.get(`/secrets/${id}`)
    return resp.data as Secret
  },

  async update(id: string, data: { value: string }): Promise<void> {
    await apiClient.put(`/secrets/${id}`, data)
  },

  async delete(id: string): Promise<void> {
    await apiClient.delete(`/secrets/${id}`)
  },
}
