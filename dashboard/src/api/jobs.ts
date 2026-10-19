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
  /** Manual trigger user id (null for webhook/cron/git). */
  trigger_user_id?: string | null
  /** GHA-style per-pipeline build number (1-based, allocated at trigger). */
  run_number?: number
  /** Re-run counter; always 1 until job re-run exists. */
  run_attempt?: number
  /** Append-only approval trail (approve/reject/cancel/timeout). */
  approvals?: JobApprovalRecord[]
}

export interface JobApprovalRecord {
  id: string
  user_id: string
  username: string
  action: 'approve' | 'reject' | 'cancel' | 'timeout' | string
  comment: string
  created_at?: string | null
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
  /** Platform Environment slug selected at trigger (Variable overlays). */
  environment?: string | null
  /** Frozen Environment protection policy (when protected). */
  protection?: {
    enabled?: boolean
    min_trigger_role?: string
    min_approve_role?: string
    require_approvals?: number
    forbid_self_approve?: boolean
    approval_timeout_seconds?: number
  } | null
  /** Approval progress when status is waiting_approval. */
  approval?: {
    required?: boolean
    received?: number
    outcome?: string
    by?: string
    at?: string
    reason?: string
  } | null
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
  /** Frozen execution capability requests inherited from a locked plugin. */
  capabilities?: string[]
  marketplace?: {
    uses_ref: string
    resolved_version: string
    content_sha256: string
    source: string
    reviewed: boolean
    capabilities?: string[]
    requires_buz?: string | null
    inputs: Array<{
      name: string
      description?: string | null
      required?: boolean | null
      default?: string | null
      masking?: boolean | null
      type?: string | null
      pattern?: string | null
      options?: string[] | null
    } | Record<string, string>>
  }
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

/** One day of the Overview build-trend chart (zero-filled by the backend). */
export interface DailyBuildPoint {
  date: string
  total: number
  success: number
  failed: number
  cancelled: number
  pending: number
  other: number
}

export interface JobTrend {
  days: number
  points: DailyBuildPoint[]
}

/** Lightweight entry for the Overview build stream (no snapshot/logs). */
export interface RecentJob {
  id: string
  pipeline_id: string
  pipeline_name: string | null
  status: string
  trigger_by: string
  team_id: string
  created_at: string | null
}

export const jobsApi = {
  async list(): Promise<Job[]> {
    const resp = await apiClient.get('/jobs')
    return resp.data
  },

  /** Daily build counts for the last 7/15/30 days, team- and role-scoped. */
  async trend(opts?: { days?: number; teamId?: string }): Promise<JobTrend> {
    const params: Record<string, string | number> = { days: opts?.days ?? 7 }
    if (opts?.teamId) params.team_id = opts.teamId
    const resp = await apiClient.get('/jobs/stats/trend', { params })
    return resp.data
  },

  /** Recent builds for the Overview sidebar stream. */
  async recent(opts?: { limit?: number; teamId?: string }): Promise<RecentJob[]> {
    const params: Record<string, string | number> = { limit: opts?.limit ?? 10 }
    if (opts?.teamId) params.team_id = opts.teamId
    const resp = await apiClient.get('/jobs/recent', { params })
    return resp.data
  },

  async get(id: string): Promise<any> {
    const resp = await apiClient.get(`/jobs/${id}`)
    return resp.data
  },

  async cancel(id: string): Promise<void> {
    await apiClient.post(`/jobs/${id}/cancel`)
  },

  async approve(id: string, comment?: string): Promise<{ status: string; approvals?: number }> {
    const resp = await apiClient.post(`/jobs/${id}/approve`, { comment: comment || '' })
    return resp.data
  },

  async reject(id: string, comment?: string): Promise<{ status: string }> {
    const resp = await apiClient.post(`/jobs/${id}/reject`, { comment: comment || '' })
    return resp.data
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
