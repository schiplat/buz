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
  /** The template's `requires_buz` declaration, if it made one. */
  requires_buz: string | null
  /** Three-state compatibility with this manager build: null = not checked
   *  yet (transient fetch failure at sync), false = cannot be enabled here. */
  buz_compatible: boolean | null
  /** The gate-identical error behind `buz_compatible === false`. */
  compat_error: string | null
}

export interface LocalTemplate {
  id: string
  version: string
  file: string
  reviewed: boolean
  engine?: string
  description?: string
}

export interface SyncProgressState {
  phase: 'index' | 'processing' | 'done' | 'failed' | 'idle'
  total_plugins?: number
  done_plugins?: number
  total_entries?: number
  done_entries?: number
  error?: string | null
}

export const stepSourcesApi = {
  async listSources(): Promise<{ items: StepSource[] }> {
    const resp = await apiClient.get('/admin/step-sources')
    return resp.data as { items: StepSource[] }
  },
  async createSource(name: string, url: string, autoEnable: boolean): Promise<{ source: StepSource }> {
    // Returns immediately — the sync runs in the background and the UI polls
    // the list for last_synced_at / last_sync_error.
    const resp = await apiClient.post('/admin/step-sources', { name, url, auto_enable: autoEnable })
    return resp.data as { source: StepSource }
  },
  async syncSource(id: string): Promise<void> {
    await apiClient.post(`/admin/step-sources/${id}/sync`)
  },
  /** Live progress for a running background sync (progress bar). */
  async getSyncProgress(id: string): Promise<SyncProgressState> {
    const resp = await apiClient.get(`/admin/step-sources/${id}/sync-progress`)
    return resp.data as SyncProgressState
  },
  async deleteSource(id: string): Promise<void> {
    await apiClient.delete(`/admin/step-sources/${id}`)
  },
  async listPool(): Promise<{ items: PoolEntry[] }> {
    const resp = await apiClient.get('/admin/step-pool')
    return resp.data as { items: PoolEntry[] }
  },
  async refreshPlugin(namespace: string, stepId: string): Promise<{ items: PoolEntry[] }> {
    const resp = await apiClient.post(`/admin/step-pool/${namespace}/${stepId}/refresh`)
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
