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
  /** Review-gate posture; absent on managers older than the banner feature. */
  plugin_review?: StepReviewHealth
  database: DatabaseHealth
  jobs: JobsBacklogHealth
  ws: WsHealth
}

export interface StepReviewHealth {
  manifest_configured: boolean
  require_reviewed: boolean
}

export const DEFAULT_MARKETPLACE_URL = 'https://plugins.buz.schiplat.com'

let healthOnce: Promise<HealthInfo | null> | null = null

/**
 * One `/health` round-trip per page load, shared by every consumer (sidebar
 * version label, marketplace URL, review-gate banner). Resolves null when the
 * probe fails so callers can keep their fallbacks.
 */
function fetchHealthOnce(): Promise<HealthInfo | null> {
  if (!healthOnce) {
    healthOnce = healthApi.get().catch(() => null)
  }
  return healthOnce
}

/**
 * Plugin-marketplace URL configured on the Manager (falls back to the public
 * marketplace). Failures keep the default so the YAML editor link works even
 * when the health probe is unreachable.
 */
export async function getMarketplaceUrl(): Promise<string> {
  const info = await fetchHealthOnce()
  return info?.marketplace_url ?? DEFAULT_MARKETPLACE_URL
}

/**
 * Step review-gate posture for the "gate not enabled" banner. Null when the
 * probe fails or the manager predates the field — the banner stays silent.
 */
export async function getStepReviewGate(): Promise<StepReviewHealth | null> {
  const info = await fetchHealthOnce()
  return info?.plugin_review ?? null
}

export const healthApi = {
  async get(): Promise<HealthInfo> {
    // Health probe lives at the root path (outside the /api/v1 prefix) so
    // load balancers / orchestrators can reach it without a rewrite rule.
    const resp = await apiClient.get('/health', { baseURL: '' })
    return resp.data
  },
}
