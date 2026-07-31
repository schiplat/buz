import apiClient from './client'

export interface AuditLogItem {
  id: string
  created_at: string
  actor_user_id: string
  actor_username: string
  action: string
  resource_type: string
  resource_id: string
  resource_name: string
  team_id: string | null
  before: unknown
  after: unknown
  meta: unknown
  ip: string
}

export interface AuditLogListResult {
  items: AuditLogItem[]
  total: number
  page: number
  page_size: number
}

export interface AuditLogListParams {
  page?: number
  page_size?: number
  action?: string
  actor?: string
  resource_type?: string
  q?: string
  from?: string
  to?: string
}

export const auditApi = {
  async list(params: AuditLogListParams = {}): Promise<AuditLogListResult> {
    const resp = await apiClient.get('/audit-logs', { params })
    return resp.data as AuditLogListResult
  },
}
