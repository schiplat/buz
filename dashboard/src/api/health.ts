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
  marketplace_url?: string
  uptime_seconds: number
  started_at?: string
  database: DatabaseHealth
  jobs: JobsBacklogHealth
  ws: WsHealth
}

export const DEFAULT_MARKETPLACE_URL = 'https://plugins.buz.schiplat.com'

let cachedMarketplaceUrl: string | null = null

/**
 * Plugin-marketplace URL configured on the Manager (falls back to the public
 * marketplace). Fetched once per page load; failures keep the default so the
 * YAML editor link works even when the health probe is unreachable.
 */
export async function getMarketplaceUrl(): Promise<string> {
  if (cachedMarketplaceUrl) return cachedMarketplaceUrl
  try {
    const info = await healthApi.get()
    if (info.marketplace_url) cachedMarketplaceUrl = info.marketplace_url
  } catch {
    // keep the default — the link should not break on a failed probe
  }
  return cachedMarketplaceUrl ?? DEFAULT_MARKETPLACE_URL
}

export const healthApi = {
  async get(): Promise<HealthInfo> {
    // Health probe lives at the root path (outside the /api/v1 prefix) so
    // load balancers / orchestrators can reach it without a rewrite rule.
    const resp = await apiClient.get('/health', { baseURL: '' })
    return resp.data
  },
}
