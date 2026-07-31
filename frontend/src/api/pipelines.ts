import apiClient from './client'

export interface Pipeline {
  id: string
  name: string
  description: string
  trigger_type: string
  runner_labels: string[]
  timeout_seconds: number
  concurrency_group: string | null
  concurrency_mode: string | null
  concurrency_max_queue: number | null
  concurrency_queue_timeout_seconds: number | null
  source: string
  input_defs: PipelineInputDef[] | null
  is_active: boolean
  owner_group: string
  team_id?: string
  secret_ids: string[]
  latest_job_id: string | null
  latest_job_status: string | null
  latest_job_start: string | null
  /** Total jobs ever created for this pipeline. */
  job_total: number
  /** Jobs with status === 'success'. */
  job_success: number
  /** job_success / job_total; null when job_total === 0. */
  success_rate: number | null
  created_at: string
  updated_at: string
}

export interface CreatePipelinePayload {
  name: string
  description?: string
  trigger_type: string
  webhook_path?: string
  cron_expr?: string
  runner_labels: string[]
  timeout_seconds?: number
  concurrency_group?: string
  concurrency_mode?: string
  concurrency_max_queue?: number
  concurrency_queue_timeout_seconds?: number
  owner_group?: string
  team_id?: string
  var_ids?: string[]
  secret_ids?: string[]
  env?: { name: string; value: string }[]
  steps: StepDef[]
  input_defs?: PipelineInputDef[]
}

export interface PipelineInputDef {
  name: string
  description?: string
  required?: boolean
  default?: string
  /** When true, values are redacted in logs, audit, and job snapshots (still injected as env). */
  masking?: boolean
}

export interface StepDef {
  name: string
  run: string
  uses?: string
  image?: string
  /** Extra host→container bind mounts when uses: docker. Each entry is "host_path:container_path". */
  volumes?: string[]
  host?: string | string[]
  host_labels?: string | string[]
  dir?: string
  timeout?: string
  retry?: number
  continue_on_error?: boolean
  /** Step-level secret names (resolved to values at runtime). */
  secrets?: string[]
  /** Run the Docker container in privileged mode (DinD). */
  privileged?: boolean
  /** Manager Secret name for docker login (pull/push). Shell / SSH / Docker. */
  registry_secret?: string
}

export interface YamlValidationResult {
  valid: boolean
  name: string | null
  errors: YamlValidationError[]
  warnings: string[]
  parsed?: YamlParsedData | null
}

export interface YamlParsedData {
  description?: string | null
  trigger_type?: string | null
  trigger_config?: YamlTriggerConfig | null
  runner?: YamlRunnerConfig | null
  concurrency?: string | null
  concurrency_config?: YamlConcurrencyConfig | null
  env?: Record<string, string> | null
  variables?: string[] | null
  secrets?: string[] | null
  steps: YamlStepData[]
}

export interface YamlTriggerConfig {
  webhook_path?: string | null
  cron_expr?: string | null
  inputs?: YamlInputData[] | null
}

export interface YamlInputData {
  name: string
  description?: string | null
  required?: boolean | null
  default?: string | null
  masking?: boolean | null
}

export interface YamlRunnerConfig {
  labels?: string[] | null
  timeout?: string | null
}

export interface YamlConcurrencyConfig {
  group: string
  mode?: string | null
  max_queue?: number | null
  queue_timeout?: string | null
}

export interface YamlStepData {
  name?: string | null
  run?: string | null
  uses?: string | null
  image?: string | null
  /** Comma-separated "host:container" pairs from YAML volumes array. */
  volumes?: string | null
  host?: string | null
  host_labels?: string | null
  dir?: string | null
  timeout?: string | null
  retry?: number | null
  continue_on_error?: boolean | null
  secrets?: string[] | null
  /** Privileged mode for docker engine steps. */
  privileged?: boolean | null
  /** Manager Secret name for docker login (pull/push). */
  registry_secret?: string | null
}

export interface YamlValidationError {
  field: string
  message: string
}

export const pipelinesApi = {
  async list(): Promise<Pipeline[]> {
    const resp = await apiClient.get('/pipelines')
    return resp.data
  },

  async create(payload: CreatePipelinePayload): Promise<any> {
    const resp = await apiClient.post('/pipelines', payload)
    return resp.data
  },

  async get(id: string): Promise<any> {
    const resp = await apiClient.get(`/pipelines/${id}`)
    return resp.data
  },

  async update(id: string, payload: CreatePipelinePayload): Promise<any> {
    const resp = await apiClient.put(`/pipelines/${id}`, payload)
    return resp.data
  },

  async trigger(id: string, inputs?: Record<string, string>): Promise<{ job_id: string }> {
    const resp = await apiClient.post(`/pipelines/${id}/trigger`, { inputs: inputs || {} })
    return resp.data
  },

  async delete(id: string): Promise<void> {
    await apiClient.delete(`/pipelines/${id}`)
  },

  async toggleActive(id: string): Promise<{ is_active: boolean }> {
    const resp = await apiClient.patch(`/pipelines/${id}/toggle-active`)
    return resp.data
  },

  async validateYaml(yaml: string): Promise<YamlValidationResult> {
    const resp = await apiClient.post('/pipelines/validate', { yaml })
    return resp.data
  },

  async listTriggerEvents(
    id: string,
    params?: { source?: string; decision?: string; page?: number; page_size?: number },
  ): Promise<TriggerEventsPage> {
    const resp = await apiClient.get(`/pipelines/${id}/trigger-events`, { params })
    return resp.data
  },

  async listGitHooks(id: string): Promise<GitHookConfig[]> {
    const resp = await apiClient.get(`/pipelines/${id}/git-hooks`)
    return resp.data?.items || []
  },

  async createGitHook(
    id: string,
    payload: {
      provider: 'github' | 'gitee'
      label?: string
      branch_filter?: string
      enabled?: boolean
    },
  ): Promise<GitHookConfig> {
    const resp = await apiClient.post(`/pipelines/${id}/git-hooks`, payload)
    return resp.data
  },

  async deleteGitHook(id: string, hookId: string): Promise<void> {
    await apiClient.delete(`/pipelines/${id}/git-hooks/${hookId}`)
  },
}

export interface TriggerEvent {
  id: string
  source: string
  provider: string | null
  event_type: string
  delivery_id: string | null
  repo_full_name: string
  ref: string
  branch: string
  tag: string
  commit_sha: string
  commit_message: string
  sender: string
  compare_url: string
  received_at: string
  decision: string
  skip_reason: string
  job_id: string | null
  job_status: string | null
}

export interface TriggerEventsPage {
  items: TriggerEvent[]
  total: number
  page: number
  page_size: number
}

export interface GitHookConfig {
  id: string
  label: string
  provider: 'github' | 'gitee' | string
  hook_key: string
  url_path: string
  branch_filter: string
  enabled: boolean
  has_secret: boolean
  secret?: string
  secret_shown_once?: boolean
}
