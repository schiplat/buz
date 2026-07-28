import apiClient from './client'

export interface Host {
  id: string
  name: string
  host: string
  port: number
  username: string
  credential_type: string
  credential_data?: string
  credential_stored?: boolean
  labels: string[]
  team_id?: string | null
  created_at: string
  updated_at: string
}

export const hostsApi = {
  async list(): Promise<Host[]> {
    const resp = await apiClient.get('/hosts')
    return resp.data as Host[]
  },

  async create(data: {
    name: string
    host: string
    port: number
    username: string
    credential_type?: string
    credential_data?: string
    credential_passphrase?: string
    labels?: string[]
    team_id?: string | null
  }): Promise<{ id: string; name: string }> {
    const resp = await apiClient.post('/hosts', data)
    return resp.data as { id: string; name: string }
  },

  async get(id: string): Promise<Host> {
    const resp = await apiClient.get(`/hosts/${id}`)
    return resp.data as Host
  },

  async update(id: string, data: Partial<{
    name: string
    host: string
    port: number
    username: string
    credential_type: string
    credential_data: string
    credential_passphrase?: string
    labels: string[]
    team_id: string | null
  }>): Promise<void> {
    await apiClient.put(`/hosts/${id}`, data)
  },

  async delete(id: string): Promise<void> {
    await apiClient.delete(`/hosts/${id}`)
  },
}
