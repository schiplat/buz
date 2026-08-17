import apiClient from './client'

export interface Job {
  id: string
  pipeline_id: string
  pipeline_name: string | null
  runner_id: string | null
  status: string
  trigger_by: string
  trigger_params: string | null
  snapshot: JobSnapshot | null
  step_statuses: StepStatus[] | null
  retry_times: number
  start_time: string | null
  end_time: string | null
  created_at: string
  updated_at: string
  /** Pipeline team — used by the top-bar team filter. */
  team_id?: string
  /** Admin-only debug run: logs are unredacted, kept 24h, admin-readable only. */
  debug_unredact?: boolean
  /** Verbose execution mode: extra per-step detail in logs, redaction intact. */
  verbose_logs?: boolean
}

export interface StepStatus {
  step_order: number | string
  status: string
  started_at?: string
  finished_at?: string
}

export interface JobSnapshot {
  pipeline: {
    trigger_type: string
    timeout_seconds: number
    source: string
  }
  runner_labels: string[]
  /** Frozen at trigger — concurrency group name (null/empty = no concurrency). */
  concurrency_group: string | null
  /** Effective mode at trigger: queue | discard-old | discard-new. */
  concurrency_mode?: string | null
  concurrency_max_queue?: number | null
  concurrency_queue_timeout_seconds?: number | null
  tasks: SnapshotTask[]
  env: SnapshotEnv[]
  inputs: Record<string, string> | null
  /** Input names stored encrypted / shown as *** */
  masking_input_keys?: string[] | null
  triggered_by: string
  triggered_at: string
}

export interface SnapshotTask {
  step_order: number
  name: string
  exec_type: string
  host_id: string | null
  command_template: string
  work_dir: string
  retry_count: number
  timeout_seconds: number
  fail_strategy: string
}

export interface SnapshotEnv {
  name: string
  is_secret: boolean
}

export interface LogChunk {
  chunk_index: number
  step_order: number
  content: string
  created_at?: string | null
}

export const jobsApi = {
  async list(): Promise<Job[]> {
    const resp = await apiClient.get('/jobs')
    return resp.data
  },

  async get(id: string): Promise<any> {
    const resp = await apiClient.get(`/jobs/${id}`)
    return resp.data
  },

  async cancel(id: string): Promise<void> {
    await apiClient.post(`/jobs/${id}/cancel`)
  },

  async delete(id: string): Promise<void> {
    await apiClient.delete(`/jobs/${id}`)
  },

  async logs(id: string, opts?: { signal?: AbortSignal; timeout?: number }): Promise<LogChunk[]> {
    const resp = await apiClient.get(`/jobs/${id}/logs`, {
      signal: opts?.signal,
      timeout: opts?.timeout,
    })
    return resp.data
  },
}
