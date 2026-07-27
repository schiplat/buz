import apiClient from './client'

export const authApi = {
  async login(username: string, password: string): Promise<{ token: string }> {
    const resp = await apiClient.post('/auth/login', { username, password })
    return resp.data
  },
}
