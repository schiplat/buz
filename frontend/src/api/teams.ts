import apiClient from './client'

export interface Team {
  id: string
  name: string
  slug: string
  description?: string
  my_role?: string | null
  created_at?: string
  updated_at?: string
  members?: TeamMember[]
}

export interface TeamMember {
  user_id: string
  username: string
  display_name: string
  role: string
}

export const teamsApi = {
  async list(): Promise<Team[]> {
    const resp = await apiClient.get('/teams')
    return resp.data as Team[]
  },
  async get(id: string): Promise<Team> {
    const resp = await apiClient.get(`/teams/${id}`)
    return resp.data as Team
  },
  async create(data: { name: string; slug: string; description?: string }): Promise<Team> {
    const resp = await apiClient.post('/teams', data)
    return resp.data as Team
  },
  async update(id: string, data: { name?: string; description?: string }): Promise<void> {
    await apiClient.put(`/teams/${id}`, data)
  },
  async delete(id: string): Promise<void> {
    await apiClient.delete(`/teams/${id}`)
  },
  async upsertMember(teamId: string, data: { user_id: string; role: string }): Promise<void> {
    await apiClient.put(`/teams/${teamId}/members`, data)
  },
  async removeMember(teamId: string, userId: string): Promise<void> {
    await apiClient.delete(`/teams/${teamId}/members/${userId}`)
  },
}
