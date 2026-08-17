import apiClient from './client'

export interface HealthInfo {
  status: string
  name: string
  version: string
  component: string
}

export const healthApi = {
  async get(): Promise<HealthInfo> {
    const resp = await apiClient.get('/health')
    return resp.data
  },
}
