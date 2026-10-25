import apiClient from './client'

/** Step sources + curated pool (step-marketplace §6.8). Admin-managed;
 *  the library view lists what pipeline authors may `uses:` today. */

export interface StepSource {
  id: string
  name: string
  url: string
  auto_enable: boolean
  enabled: boolean
  last_synced_at: string | null
  last_sync_error: string | null
}

export interface PoolEntry {
  namespace: string
  step_id: string
  version: string
  sha256: string
  source_id: string
  source_name: string | null
  file_path: string
  enabled: boolean
  reviewed: boolean
  added_by: string | null
}

export interface LocalTemplate {
  id: string
  version: string
  file: string
  reviewed: boolean
  engine?: string
  description?: string
}

export const stepSourcesApi = {
  async listSources(): Promise<{ items: StepSource[] }> {
    const resp = await apiClient.get('/admin/step-sources')
    return resp.data as { items: StepSource[] }
  },
  async createSource(name: string, url: string, autoEnable: boolean): Promise<void> {
    // Returns immediately — the sync runs in the background and the UI polls
    // the list for last_synced_at / last_sync_error.
    await apiClient.post('/admin/step-sources', { name, url, auto_enable: autoEnable })
  },
  async syncSource(id: string): Promise<void> {
    await apiClient.post(`/admin/step-sources/${id}/sync`)
  },
  async deleteSource(id: string): Promise<void> {
    await apiClient.delete(`/admin/step-sources/${id}`)
  },
  async listPool(): Promise<{ items: PoolEntry[] }> {
    const resp = await apiClient.get('/admin/step-pool')
    return resp.data as { items: PoolEntry[] }
  },
  async togglePoolEntry(
    namespace: string,
    stepId: string,
    version: string,
    enabled: boolean,
  ): Promise<void> {
    await apiClient.put(`/admin/step-pool/${namespace}/${stepId}/${version}`, { enabled })
  },
}

/** Local (local_dir) templates from GET /api/v1/steps. */
export const stepsApi = {
  async listLocal(): Promise<{ items: Array<Record<string, unknown>>; unparseable: unknown[] }> {
    const resp = await apiClient.get('/steps')
    return resp.data as { items: Array<Record<string, unknown>>; unparseable: unknown[] }
  },
}
