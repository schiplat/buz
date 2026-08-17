// User-facing types for the Buz frontend

export type RunnerStatus = 'online' | 'offline' | 'busy'
export type TriggerType = 'manual' | 'webhook' | 'cron'
export type JobStatus = 'pending' | 'running' | 'success' | 'failed' | 'cancelled' | 'timeout'
export type ExecType = 'local' | 'ssh' | 'docker' | 'wasm'

export interface Runner {
  id: string
  name: string
  labels: string[]
  status: RunnerStatus
  version: string | null
  lastSeen: string | null
  createdAt: string
  updatedAt: string
}

export interface Host {
  id: string
  name: string
  host: string
  port: number
  username: string
  credentialType: 'vault' | 'key_path' | 'agent'
  labels: string[]
  createdAt: string
}

export interface Pipeline {
  id: string
  name: string
  description: string
  triggerType: TriggerType
  webhookPath: string | null
  cronExpr: string | null
  runnerLabels: string[]
  timeoutSeconds: number
  concurrencyGroup: string | null
  isActive: boolean
  createdAt: string
  steps: Step[]
}

export interface Step {
  id: string
  stepOrder: number
  name: string
  execType: ExecType
  hostId: string | null
  commandTemplate: string
  workDir: string
  retryCount: number
  timeoutSeconds: number
  failStrategy: 'stop' | 'continue' | 'ignore'
  continueOnError: boolean
}

export interface Job {
  id: string
  pipelineId: string
  runnerId: string | null
  status: JobStatus
  triggerBy: string
  startTime: string | null
  endTime: string | null
  retryTimes: number
  createdAt: string
}

export interface CreatePipelineRequest {
  name: string
  description?: string
  triggerType: TriggerType
  webhookPath?: string
  cronExpr?: string
  runnerLabels: string[]
  timeoutSeconds?: number
  concurrencyGroup?: string
  env?: { name: string; value: string }[]
  secrets?: string[]
  steps: CreateStepDef[]
}

export interface CreateStepDef {
  name: string
  run: string
  uses?: ExecType
  host?: string
  dir?: string
  timeout?: string
  retry?: number
  continueOnError?: boolean
}

export interface PaginatedData<T> {
  items: T[]
  total: number
  page: number
  pageSize: number
}

export interface ApiResponse<T> {
  code: number
  message: string
  data: T
}
