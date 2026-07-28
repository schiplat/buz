import apiClient from './client'

export interface TeamMembership {
  id: string
  slug: string
  name: string
  role: string
}

export interface MeResponse {
  id: string
  username: string
  is_platform_admin: boolean
  teams: TeamMembership[]
}

export const authApi = {
  async login(
    username: string,
    password: string,
  ): Promise<{
    token: string
    user_id?: string
    username?: string
    is_platform_admin?: boolean
  }> {
    const resp = await apiClient.post('/auth/login', { username, password })
    return resp.data
  },

  async me(): Promise<MeResponse> {
    const resp = await apiClient.get('/auth/me')
    return resp.data as MeResponse
  },

  async verify(): Promise<void> {
    await apiClient.get('/auth/verify')
  },

  async changePassword(currentPassword: string, newPassword: string): Promise<void> {
    await apiClient.post('/auth/change-password', {
      current_password: currentPassword,
      new_password: newPassword,
    })
  },
}
