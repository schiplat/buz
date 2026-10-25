import apiClient from './client'

/** Platform-level admin-editable settings (step-marketplace §6.8 Phase 0). */
export type SettingKind = 'url' | 'bool' | 'path' | 'number'

export interface PlatformSetting {
  key: string
  kind: SettingKind
  /** Effective value: DB override when present, else the manager.toml default. */
  value: string
  default: string
  overridden: boolean
}

export const adminSettingsApi = {
  async list(): Promise<{ items: PlatformSetting[] }> {
    const resp = await apiClient.get('/admin/settings')
    return resp.data as { items: PlatformSetting[] }
  },
  async update(key: string, value: string): Promise<{ key: string; value: string }> {
    const resp = await apiClient.put(`/admin/settings/${key}`, { value })
    return resp.data as { key: string; value: string }
  },
  async reset(key: string): Promise<{ key: string }> {
    const resp = await apiClient.delete(`/admin/settings/${key}`)
    return resp.data as { key: string }
  },
}
