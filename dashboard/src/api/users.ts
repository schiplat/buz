import apiClient from './client'

export interface User {
  id: string
  username: string
  display_name: string
  is_platform_admin: boolean
  is_super_admin: boolean
  is_active: boolean
  created_at?: string
  updated_at?: string
}

export const usersApi = {
  async list(): Promise<User[]> {
    const resp = await apiClient.get('/users')
    return resp.data as User[]
  },
  async create(data: {
    username: string
    password: string
    display_name?: string
    is_platform_admin?: boolean
    is_super_admin?: boolean
  }): Promise<User> {
    const resp = await apiClient.post('/users', data)
    return resp.data as User
  },
  async update(
    id: string,
    data: {
      password?: string
      display_name?: string
      is_platform_admin?: boolean
      is_super_admin?: boolean
      is_active?: boolean
    },
  ): Promise<void> {
    await apiClient.put(`/users/${id}`, data)
  },
  async delete(id: string): Promise<void> {
    await apiClient.delete(`/users/${id}`)
  },
}
