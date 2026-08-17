import apiClient from './client'

export interface Variable {
  id: string
  name: string
  value: string
  updated_at: string
}

export const variablesApi = {
  async list(): Promise<Variable[]> {
    const resp = await apiClient.get('/variables')
    return resp.data as Variable[]
  },

  async create(data: { name: string; value: string }): Promise<{ id: string; name: string }> {
    const resp = await apiClient.post('/variables', data)
    return resp.data as { id: string; name: string }
  },

  async get(id: string): Promise<Variable> {
    const resp = await apiClient.get(`/variables/${id}`)
    return resp.data as Variable
  },

  async update(id: string, data: { value: string }): Promise<void> {
    await apiClient.put(`/variables/${id}`, data)
  },

  async delete(id: string): Promise<void> {
    await apiClient.delete(`/variables/${id}`)
  },
}
