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
  concurrency_group: string | null
  tasks: SnapshotTask[]
  env: SnapshotEnv[]
  inputs: Record<string, string> | null
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
  created_at: string
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
