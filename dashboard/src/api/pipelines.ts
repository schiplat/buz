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
  /** Basename under Manager `yaml_scan_dir` when `source=yaml` (flat dir, no subfolders). */
  yaml_path?: string | null
  /** Slugs this pipeline may target; every trigger must choose one. */
  environments: string[]
  /** On-disk YAML vs DB projection sync (only meaningful for source=yaml). */
  yaml_sync?: YamlSyncStatus | null
  input_defs: PipelineInputDef[] | null
  is_active: boolean
  owner_group: string
  team_id?: string
  secret_ids: string[]
  /** Inline pipeline env vars (`env:` block), for the edit form. */
  env: { name: string; value: string }[]
  latest_job_id: string | null
  latest_job_status: string | null
  latest_job_start: string | null
  /** Detail API tasks include inherited plugin capabilities (requests, not grants). */
  tasks?: PipelineTask[]
  /** Total jobs ever created for this pipeline. */
  job_total: number
  /** Jobs with status === 'success'. */
  job_success: number
  /** job_success / job_total; null when job_total === 0. */
  success_rate: number | null
  created_at: string
  updated_at: string
}

export interface YamlFileWriteResult {
  pipeline_id: string
  filename: string
  yaml_path: string
  name: string
  etag?: string
}

export interface YamlSourcePayload {
  pipeline_id: string
  name: string
  filename: string
  yaml_path: string
  yaml: string
  etag?: string
}

/** On-disk YAML vs DB projection — from GET /pipelines/{id}. */
export interface YamlSyncStatus {
  filename: string
  status: 'in_sync' | 'stale' | 'validation_failed' | 'missing_file' | 'not_file_backed' | string
  last_synced_at?: string | null
  file_mtime?: string | null
  etag?: string | null
  errors?: string[]
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
  environments: string[]
  var_ids?: string[]
  secret_ids?: string[]
  env?: { name: string; value: string }[]
  steps: PipelineStepInput[]
  input_defs?: PipelineInputDef[]
}

export interface PipelineInputDef {
  name: string
  description?: string
  required?: boolean
  default?: string
  /** When true, values are redacted in logs, audit, and job snapshots (still injected as env). */
  masking?: boolean
  /** Optional value constraint: string / number / boolean / host / choice. */
  type?: string
  /** ECMAScript-compatible regex the resolved value must match. */
  pattern?: string
  /** Allowed values for `type: choice`. */
  options?: string[]
}

export interface PipelineStepInput {
  name: string
  /** A pinned plugin template ref; capability requests come only from its locked metadata. */
  uses?: string
  with?: Record<string, string>
  run?: string
  engine?: string
  image?: string
  /** Extra host→container bind mounts when uses: docker. Each entry is "host_path:container_path". */
  volumes?: string[]
  host?: string | string[]
  host_labels?: string | string[]
  /** SSH multi-host fan-out: `chain` (default) or `parallel`. */
  hosts_strategy?: 'chain' | 'parallel' | string
  dir?: string
  timeout?: string
  retry?: number
  continue_on_error?: boolean
  /** Step secret allowlist (optional narrowing). Does not auto-inject `$NAME`. */
  secrets?: string[]
  /**
   * Step env map. Secret values must be mapped explicitly, e.g.
   * `{ GIT_SSH_KEY: '${secrets.GIT_SSH_KEY}' }`, then use `$GIT_SSH_KEY` in `run`.
   */
  env?: Record<string, string>
  /** Run the Docker container in privileged mode (DinD). */
  privileged?: boolean
  /** Purpose slot: `${secrets.NAME}` or bare name for docker login. */
  registry_secret?: string
  /** Purpose slot: `${secrets.NAME}` or bare OSS auth secret name. */
  config_source?: string
  /** OSS object-key prefix; Manager appends configs.tar.gz when signing. */
  config_key_path?: string
  /** Unpack / working directory → $BUZ_CONFIG_DIR. */
  config_dest?: string
  /** Host-authorable form fields only; capability and uses metadata are server/plugin-owned. */
  config_render?: Array<{
    template?: string
    output?: string
    mode?: string
    on_missing?: string
    on_change?: string
    services?: string[]
    backup_keep?: number
  }>
}

export type StepDef = PipelineStepInput

export interface ResolvedMarketplaceStep {
  uses_ref: string
  resolved_version?: string
  content_sha256?: string
  source?: string
  reviewed?: boolean
  /** Exposed for resolved step details and frozen job history. */
  capabilities?: string[]
  requires_buz?: string | null
  inputs: Array<YamlInputData | Record<string, string>>
}

export interface PipelineTask extends PipelineStepInput {
  id?: string
  step_order: number
  exec_type: string
  host_id?: string | null
  host_names?: string
  host_labels?: string
  command_template: string
  work_dir?: string
  retry_count?: number
  timeout_seconds?: number
  fail_strategy?: string
  step_secrets?: string
  step_env?: Record<string, string> | null
  privileged?: boolean
  capabilities?: string[]
  /** Original marketplace ref when rendered from the YAML source. */
  uses_ref?: string
  marketplace?: ResolvedMarketplaceStep
}

export interface YamlValidationResult {
  valid: boolean
  name: string | null
  errors: YamlValidationError[]
  warnings: string[]
  parsed?: YamlParsedData | null
}

export interface YamlParsedData {
  environments?: string[] | null
  description?: string | null
  trigger_type?: string | null
  trigger_config?: YamlTriggerConfig | null
  runner?: YamlRunnerConfig | null
  concurrency?: string | null
  concurrency_config?: YamlConcurrencyConfig | null
  env?: Record<string, string> | null
  variables?: string[] | null
  secrets?: string[] | null
  /** End-of-build notify channels (expressions kept). */
  notify?: YamlNotifyChannel[] | null
  steps: YamlStepData[]
}

export interface YamlNotifyChannel {
  type: string
  webhook: string
  on?: string[] | null
}

export interface YamlTriggerConfig {
  webhook_path?: string | null
  webhook_environment?: string | null
  cron_expr?: string | null
  cron_environment?: string | null
  inputs?: YamlInputData[] | null
}

export interface YamlInputData {
  name: string
  description?: string | null
  required?: boolean | null
  default?: string | null
  masking?: boolean | null
  /** Declared constraint kind: string / number / boolean / host / choice. */
  type?: string | null
  pattern?: string | null
  options?: string[] | null
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
  /** Raw uses reference is not emitted by validate YAML (server validates separately). */
  uses?: string | null
  with?: Record<string, string> | null
  run?: string | null
  engine?: string | null
  image?: string | null
  /** Comma-separated "host:container" pairs from YAML volumes array. */
  volumes?: string | null
  host?: string | null
  host_labels?: string | null
  /** SSH multi-host fan-out: `chain` (default) or `parallel`. */
  hosts_strategy?: string | null
  dir?: string | null
  timeout?: string | null
  retry?: number | null
  continue_on_error?: boolean | null
  secrets?: string[] | null
  /** Step env map (`env:` under the step); values may be `${secrets.X}`. */
  env?: Record<string, string> | null
  /** Privileged mode for docker engine steps. */
  privileged?: boolean | null
  /** Purpose slot: `${secrets.NAME}` or bare name for docker login. */
  registry_secret?: string | null
  /** Purpose slot / flattened `config.source`. */
  config_source?: string | null
  /** OSS object-key prefix; Manager appends configs.tar.gz when signing. */
  config_key_path?: string | null
  /** Unpack / working directory → $BUZ_CONFIG_DIR. */
  config_dest?: string | null
  /** Render entries flattened from nested `config.render`. */
  config_render?: Array<{
    template?: string | null
    output?: string | null
    mode?: string | null
    on_missing?: string | null
    on_change?: string | null
    services?: string[] | null
    backup_keep?: number | null
  }> | null
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

  async trigger(
    id: string,
    inputs?: Record<string, string>,
    debug?: boolean,
    verbose?: boolean,
    environment?: string | null,
  ): Promise<{ job_id: string }> {
    const resp = await apiClient.post(`/pipelines/${id}/trigger`, {
      inputs: inputs || {},
      debug: debug ?? false,
      verbose: verbose ?? false,
      ...(environment ? { environment } : {}),
    })
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

  /** List YAML files under Manager `yaml_scan_dir` (includes `scan_dir` path). */
  async listYamlFiles(): Promise<{
    scan_dir: string
    files: Array<{
      filename: string
      yaml_path: string
      pipeline_id?: string | null
      pipeline_name?: string | null
      team_id?: string | null
      is_active?: boolean | null
    }>
  }> {
    const resp = await apiClient.get('/pipelines/yaml-files')
    return resp.data
  },

  /** Create a new YAML file under the Manager scan dir and project into DB. */
  async createYamlFile(payload: {
    yaml: string
    filename?: string
  }): Promise<YamlFileWriteResult> {
    const resp = await apiClient.post('/pipelines/yaml-files', payload)
    return resp.data
  },

  /** Overwrite a YAML file and immediately upsert the DB projection. */
  async updateYamlFile(
    filename: string,
    payload: { yaml: string; pipeline_id?: string; etag?: string },
  ): Promise<YamlFileWriteResult> {
    const resp = await apiClient.put(
      `/pipelines/yaml-files/${encodeURIComponent(filename)}`,
      payload,
    )
    return resp.data
  },

  async getYamlFile(filename: string): Promise<{
    filename: string
    yaml_path: string
    yaml: string
    etag?: string
  }> {
    const resp = await apiClient.get(`/pipelines/yaml-files/${encodeURIComponent(filename)}`)
    return resp.data
  },

  /** Load file-backed YAML for an existing `source=yaml` pipeline. */
  async getYamlSource(pipelineId: string): Promise<YamlSourcePayload> {
    const resp = await apiClient.get(`/pipelines/${pipelineId}/yaml-source`)
    return resp.data
  },

  async deleteYamlFile(filename: string): Promise<void> {
    await apiClient.delete(`/pipelines/yaml-files/${encodeURIComponent(filename)}`)
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
      environment: string
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
  environment: string
  enabled: boolean
  has_secret: boolean
  secret?: string
  secret_shown_once?: boolean
}
