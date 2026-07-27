import apiClient from './client'

export interface NetworkInterfaceInfo {
  name: string
  mac: string
  ips: string[]
}

export interface SystemInfo {
  runner_version: string
  hostname: string
  os: string
  kernel_version: string
  cpu_arch: string
  cpu_cores: number
  cpu_model: string
  cpu_usage_percent: number
  memory_total_mb: number
  memory_used_mb: number
  swap_total_mb: number
  swap_used_mb: number
  disk_total_gb: number
  disk_used_gb: number
  network_interfaces: NetworkInterfaceInfo[]
  ip_addresses: string[]
  uptime_seconds: number
}

export interface Runner {
  id: string
  name: string
  labels: string[]
  status: string
  version: string | null
  system_info: SystemInfo | null
  is_frozen: boolean
  last_seen: string | null
  created_at: string
  updated_at: string
}

export interface CreateRunnerResponse {
  id: string
  name: string
  register_token: string
}

export const runnersApi = {
  async list(): Promise<Runner[]> {
    const resp = await apiClient.get('/runners')
    return resp.data
  },

  async create(name: string, labels: string[]): Promise<CreateRunnerResponse> {
    const resp = await apiClient.post('/runners', { name, labels })
    return resp.data
  },

  async delete(id: string): Promise<void> {
    await apiClient.delete(`/runners/${id}`)
  },

  async toggleFreeze(id: string): Promise<{ is_frozen: boolean }> {
    const resp = await apiClient.patch(`/runners/${id}/freeze`)
    return resp.data
  },

  async events(id: string, page = 1, pageSize = 20): Promise<{ items: Array<{ id: string; event_type: string; message: string; created_at: string }>; total: number; page: number; page_size: number; total_pages: number }> {
    const resp = await apiClient.get(`/runners/${id}/events`, { params: { page, page_size: pageSize } })
    return resp.data
  },
}
