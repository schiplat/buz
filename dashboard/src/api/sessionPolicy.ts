import apiClient from './client'

/**
 * Session policy: server-enforced JWT lifetime plus the SPA's client-side idle
 * sign-out (docs `platform/oidc-sso.md`).
 *
 * `idle_minutes` is a convenience, not a revocation: the token keeps its TTL.
 */
export interface SessionPolicy {
  ttl_hours: number
  ttl_default_hours: number
  ttl_overridden: boolean
  /** 0 = disabled. */
  idle_minutes: number
  idle_default_minutes: number
  idle_overridden: boolean
}

export interface SessionPolicyUpdate {
  /** `null` clears the override (manager.toml `[auth] jwt_ttl_hours`). */
  ttl_hours: number | null
  /** `null` clears the override; `0` disables idle sign-out. */
  idle_minutes: number | null
}

export const sessionPolicyApi = {
  async get(): Promise<SessionPolicy> {
    const resp = await apiClient.get('/auth/session-policy')
    return resp.data as SessionPolicy
  },
  async update(payload: SessionPolicyUpdate): Promise<SessionPolicy> {
    const resp = await apiClient.put('/auth/session-policy', payload)
    return resp.data as SessionPolicy
  },
}
