import apiClient from './client'

export type EnvTeamRole = 'owner' | 'maintainer' | 'developer'

export interface Environment {
  id: string
  name: string
  slug: string
  description: string
  updated_at?: string | null
  /** Total Variable + Secret overrides (list badge). */
  override_count?: number
  var_override_count?: number
  secret_override_count?: number
  /** Approval gate (方案 B). */
  protection_enabled?: boolean
  min_trigger_role?: EnvTeamRole | string
  min_approve_role?: EnvTeamRole | string
  require_approvals?: number
  forbid_self_approve?: boolean
  approval_timeout_seconds?: number
}

export interface EnvironmentOverride {
  var_id: string
  var_name: string
  default_value: string
  value: string
  updated_at?: string | null
}

/** Secret override metadata — never includes plaintext. */
export interface EnvironmentSecretOverride {
  secret_id: string
  secret_name: string
  kind: string
  has_override: boolean
  updated_at?: string | null
}

export interface EnvironmentDetail extends Environment {
  overrides: EnvironmentOverride[]
  secret_overrides: EnvironmentSecretOverride[]
}

export type EnvironmentWritePayload = {
  name?: string
  slug?: string
  description?: string
  protection_enabled?: boolean
  min_trigger_role?: string
  min_approve_role?: string
  require_approvals?: number
  forbid_self_approve?: boolean
  approval_timeout_seconds?: number
}

export const environmentsApi = {
  async list(): Promise<Environment[]> {
    const resp = await apiClient.get('/environments')
    return resp.data as Environment[]
  },

  async get(id: string): Promise<EnvironmentDetail> {
    const resp = await apiClient.get(`/environments/${id}`)
    return resp.data as EnvironmentDetail
  },

  async create(data: {
    name: string
    slug: string
    description?: string
  } & EnvironmentWritePayload): Promise<Environment> {
    const resp = await apiClient.post('/environments', data)
    return resp.data as Environment
  },

  async update(id: string, data: EnvironmentWritePayload): Promise<Environment> {
    const resp = await apiClient.put(`/environments/${id}`, data)
    return resp.data as Environment
  },

  async delete(id: string): Promise<void> {
    await apiClient.delete(`/environments/${id}`)
  },

  async listOverrides(id: string): Promise<EnvironmentOverride[]> {
    const resp = await apiClient.get(`/environments/${id}/overrides`)
    return resp.data as EnvironmentOverride[]
  },

  async upsertOverride(
    id: string,
    data: { var_id: string; value: string },
  ): Promise<{ var_id: string; var_name: string; value: string }> {
    const resp = await apiClient.put(`/environments/${id}/overrides`, data)
    return resp.data
  },

  async deleteOverride(id: string, varId: string): Promise<void> {
    await apiClient.delete(`/environments/${id}/overrides/${varId}`)
  },

  async listSecretOverrides(id: string): Promise<EnvironmentSecretOverride[]> {
    const resp = await apiClient.get(`/environments/${id}/secret-overrides`)
    return resp.data as EnvironmentSecretOverride[]
  },

  async upsertSecretOverride(
    id: string,
    data: { secret_id: string; value: string },
  ): Promise<EnvironmentSecretOverride> {
    const resp = await apiClient.put(`/environments/${id}/secret-overrides`, data)
    return resp.data as EnvironmentSecretOverride
  },

  async deleteSecretOverride(id: string, secretId: string): Promise<void> {
    await apiClient.delete(`/environments/${id}/secret-overrides/${secretId}`)
  },
}
