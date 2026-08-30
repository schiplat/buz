import apiClient from './client'

export interface DatabasePoolHealth {
  max: number
  idle: number
  in_use: number
}

export interface DatabaseHealth {
  status: 'ok' | 'error'
  latency_ms: number
  pool: DatabasePoolHealth
}

export interface JobsBacklogHealth {
  pending: number
  running: number
  queued: number
  waiting_approval: number
}

export interface WsHealth {
  job_sinks: number
  pipeline_sinks: number
}

export interface HealthInfo {
  status: string
  name: string
  version: string
  component: string
  uptime_seconds: number
  started_at?: string
  database: DatabaseHealth
  jobs: JobsBacklogHealth
  ws: WsHealth
}

export const healthApi = {
  async get(): Promise<HealthInfo> {
    // Health probe lives at the root path (outside the /api/v1 prefix) so
    // load balancers / orchestrators can reach it without a rewrite rule.
    const resp = await apiClient.get('/health', { baseURL: '' })
    return resp.data
  },
}
