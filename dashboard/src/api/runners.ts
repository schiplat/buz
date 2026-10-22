import apiClient from './client'

export interface NetworkInterfaceInfo {
  name: string
  mac: string
  ips: string[]
}

export type ExecutionBackendStatus = 'unknown' | 'supported' | 'unsupported'

export interface RunnerExecutionCapabilities {
  container: ExecutionBackendStatus
  container_reason?: string | null
  vm: ExecutionBackendStatus
  vm_reason?: string | null
  checked_at?: string | null
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
  /**
   * Runner↔Manager clock skew in seconds (`Manager - Runner`); positive means
   * the Runner is behind. `null`/absent when not measured yet. The Runner
   * refuses to start when it exceeds its `[clock] max_skew_seconds`.
   */
  clock_skew_seconds?: number | null
  execution_capabilities?: RunnerExecutionCapabilities | null
}

export type RunnerExecutionMode = 'host' | 'container' | 'vm'

/** UI-managed runner strategy (synced to the agent via register/heartbeat). */
export interface RunnerPolicy {
  execution_mode: RunnerExecutionMode
  max_concurrent_jobs: number
  clean_workspace_after_job: boolean
  shell_enabled: boolean
  ssh_enabled: boolean
  docker_enabled: boolean
  wasm_enabled: boolean
  /** `insecure` | `known_hosts` */
  ssh_host_key_policy: string
}

export interface Runner {
  id: string
  name: string
  labels: string[]
  /** null means intentionally shared across all teams; existing legacy runners stay null. */
  team_id: string | null
  execution_mode: RunnerExecutionMode
  status: string
  version: string | null
  system_info: SystemInfo | null
  is_frozen: boolean
  /** Max parallel jobs (1–32). */
  max_concurrent_jobs: number
  clean_workspace_after_job?: boolean
  shell_enabled?: boolean
  ssh_enabled?: boolean
  docker_enabled?: boolean
  wasm_enabled?: boolean
  ssh_host_key_policy?: string
  /** Currently running jobs on this runner. */
  running_jobs?: number
  last_seen: string | null
  created_at: string
  updated_at: string
}

export interface CreateRunnerPayload {
  name: string
  labels: string[]
  /** Must be explicitly selected: null means Shared. */
  team_id: string | null
  execution_mode?: RunnerExecutionMode
  max_concurrent_jobs?: number
  clean_workspace_after_job?: boolean
  shell_enabled?: boolean
  ssh_enabled?: boolean
  docker_enabled?: boolean
  wasm_enabled?: boolean
  ssh_host_key_policy?: string
}

export interface CreateRunnerResponse {
  id: string
  name: string
  register_token: string
  max_concurrent_jobs?: number
}

export interface ReissueRegisterTokenResponse {
  register_token: string
}

export type UpdateRunnerPayload = Partial<Omit<RunnerPolicy, never>> & {
  labels?: string[]
  team_id?: string | null
}

export const runnersApi = {
  async list(): Promise<Runner[]> {
    const resp = await apiClient.get('/runners')
    return resp.data
  },

  async create(payload: CreateRunnerPayload): Promise<CreateRunnerResponse> {
    const resp = await apiClient.post('/runners', payload)
    return resp.data
  },

  async update(id: string, payload: UpdateRunnerPayload): Promise<Runner> {
    const resp = await apiClient.put(`/runners/${id}`, payload)
    return resp.data
  },

  async delete(id: string): Promise<void> {
    await apiClient.delete(`/runners/${id}`)
  },

  async toggleFreeze(id: string): Promise<{ is_frozen: boolean }> {
    const resp = await apiClient.patch(`/runners/${id}/freeze`)
    return resp.data
  },

  async restart(id: string): Promise<{ restart_requested: boolean }> {
    const resp = await apiClient.post(`/runners/${id}/restart`)
    return resp.data
  },

  async reissueRegisterToken(id: string): Promise<ReissueRegisterTokenResponse> {
    const resp = await apiClient.post(`/runners/${id}/register-token/reissue`)
    return resp.data
  },

  async events(id: string, page = 1, pageSize = 20): Promise<{ items: Array<{ id: string; event_type: string; message: string; created_at: string }>; total: number; page: number; page_size: number; total_pages: number }> {
    const resp = await apiClient.get(`/runners/${id}/events`, { params: { page, page_size: pageSize } })
    return resp.data
  },
}
