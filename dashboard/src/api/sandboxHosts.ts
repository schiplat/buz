import apiClient from './client'

/** One resource envelope on a sandbox host (values are host-owned). */
export interface ResourceProfile {
  memory: string
  cpu: string
  pids: number
  /** Scheduling weight on the host's slot budget. */
  slots: number
}

export interface TierBackend {
  /** Digest-pinned job image (repo@sha256:…). */
  image: string
  /** OCI runtime; empty = daemon default (runc). */
  runtime: string
}

export interface SandboxHostDocument {
  profiles: Record<string, ResourceProfile>
  tiers: Record<string, TierBackend>
  network: string
}

export interface SandboxHost {
  host_id: string
  instance_id: string
  status: string
  max_resource_slots: number
  desired_version: number
  applied_version: number
  applied_sha256: string
  applied_at: string | null
  last_fetch_at: string | null
  last_fetch_error: string | null
  consecutive_fetch_failures: number
  created_at: string
  updated_at: string
  document?: SandboxHostDocument | null
  attached_runners?: string[]
}

export interface ConfigVersion {
  version: number
  sha256: string
  created_by: string
  created_at: string
  document: SandboxHostDocument
}

export interface PendingChange {
  id: string
  host_id: string
  instance_id: string
  status: string
  requested_by: string
  after_document: SandboxHostDocument
  created_at: string
  decided_at: string | null
}

export interface ResourceGrant {
  team_id: string
  team_name: string
  profile: string
  created_at: string | null
}

export const sandboxHostsApi = {
  async list(): Promise<SandboxHost[]> {
    const resp = await apiClient.get('/ops/sandbox-hosts')
    return (resp.data as { hosts: SandboxHost[] }).hosts ?? []
  },

  /** Enroll a host; the token is shown exactly once (only its hash is stored). */
  async enroll(host_id: string, instance_id: string, max_resource_slots: number): Promise<{ token: string }> {
    const resp = await apiClient.post('/ops/sandbox-hosts', {
      host_id,
      instance_id,
      max_resource_slots,
    })
    return resp.data as { token: string }
  },

  async get(hostId: string, instanceId: string): Promise<SandboxHost> {
    const resp = await apiClient.get(`/ops/sandbox-hosts/${hostId}/${instanceId}`)
    return resp.data as SandboxHost
  },

  /** Replace the desired document; `desiredVersion` enables If-Match. */
  async putConfig(
    hostId: string,
    instanceId: string,
    document: SandboxHostDocument,
    desiredVersion?: number,
  ): Promise<{ pending: boolean; version?: number; change_id?: string; warnings?: string[] }> {
    const headers = desiredVersion !== undefined ? { 'If-Match': String(desiredVersion) } : undefined
    const resp = await apiClient.put(
      `/ops/sandbox-hosts/${hostId}/${instanceId}/config`,
      document,
      { headers },
    )
    return resp.data as {
      pending: boolean
      version?: number
      change_id?: string
      warnings?: string[]
    }
  },

  async validate(
    hostId: string,
    instanceId: string,
    document: SandboxHostDocument,
  ): Promise<{ valid: boolean; problems?: string[]; warnings?: string[] }> {
    const resp = await apiClient.post(
      `/ops/sandbox-hosts/${hostId}/${instanceId}/validate`,
      document,
    )
    return resp.data as { valid: boolean; problems?: string[]; warnings?: string[] }
  },

  async versions(hostId: string, instanceId: string): Promise<ConfigVersion[]> {
    const resp = await apiClient.get(`/ops/sandbox-hosts/${hostId}/${instanceId}/versions`)
    return (resp.data as { versions: ConfigVersion[] }).versions ?? []
  },

  async rollback(hostId: string, instanceId: string, version: number): Promise<{ version: number }> {
    const resp = await apiClient.post(`/ops/sandbox-hosts/${hostId}/${instanceId}/rollback`, {
      version,
    })
    return resp.data as { version: number }
  },

  async rotateToken(hostId: string, instanceId: string): Promise<{ token: string }> {
    const resp = await apiClient.post(`/ops/sandbox-hosts/${hostId}/${instanceId}/token/rotate`)
    return resp.data as { token: string }
  },

  async changes(status = 'pending'): Promise<PendingChange[]> {
    const resp = await apiClient.get(`/ops/sandbox-changes?status=${encodeURIComponent(status)}`)
    return (resp.data as { changes: PendingChange[] }).changes ?? []
  },

  async decideChange(id: string, approve: boolean, comment?: string): Promise<void> {
    await apiClient.post(`/ops/sandbox-changes/${id}/${approve ? 'approve' : 'reject'}`, {
      comment: comment ?? null,
    })
  },

  async grants(): Promise<ResourceGrant[]> {
    const resp = await apiClient.get('/ops/resource-grants')
    return (resp.data as { grants: ResourceGrant[] }).grants ?? []
  },

  async grant(teamId: string, profile: string): Promise<void> {
    await apiClient.put('/ops/resource-grants', { team_id: teamId, profile })
  },

  async revokeGrant(teamId: string, profile: string): Promise<void> {
    await apiClient.delete(`/ops/resource-grants/${teamId}/${profile}`)
  },
}
