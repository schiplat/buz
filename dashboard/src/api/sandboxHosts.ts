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
  /** Catalog profiles this host refuses (a machine that cannot serve them);
   *  everything else in the platform catalog is offered implicitly. */
  exclude_profiles?: string[]
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
  /** True once sandboxd has fetched AND applied the latest desired version. */
  pushed: boolean
  /** Server-derived liveness: sandboxd fetched config within the last 5 minutes. */
  live: boolean
  /** Push date of the currently applied version (set when the version changes). */
  applied_at: string | null
  applied_sha256: string
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

  /** Delete an enrollment; the Manager refuses while runners are still bound. */
  async remove(hostId: string, instanceId: string): Promise<void> {
    await apiClient.delete(`/ops/sandbox-hosts/${hostId}/${instanceId}`)
  },

  /** Whitelist a team for this host (zero bindings = open to every team). */
  async bindTeam(hostId: string, instanceId: string, teamId: string): Promise<void> {
    await apiClient.put(`/ops/sandbox-hosts/${hostId}/${instanceId}/teams/${teamId}`)
  },
  /** Remove a team from the host's whitelist; its jobs stop dispatching here. */
  async unbindTeam(hostId: string, instanceId: string, teamId: string): Promise<void> {
    await apiClient.delete(`/ops/sandbox-hosts/${hostId}/${instanceId}/teams/${teamId}`)
  },

  /** Resolve a tag reference to its digest-pinned form (read-only registry probe). */
  async resolveImage(image: string): Promise<string> {
    const resp = await apiClient.post('/ops/images/resolve', { image })
    return (resp.data as { pinned: string }).pinned
  },

  /** Platform defaults catalog: tiers, profiles, network, budget. */
  async getDefaults(): Promise<{
    tiers: Record<string, { image: string; runtime: string }>
    profiles: Record<string, ResourceProfile>
    network: string
    max_slots: number
  }> {
    const resp = await apiClient.get('/ops/sandbox-defaults')
    return resp.data as {
      tiers: Record<string, { image: string; runtime: string }>
      profiles: Record<string, ResourceProfile>
      network: string
      max_slots: number
    }
  },

  /** Replace the platform defaults catalog; hosts inheriting a changed value get a new desired version. */
  async putDefaults(defaults: {
    tiers: Record<string, { image: string; runtime: string }>
    profiles: Record<string, ResourceProfile>
    network: string
    max_slots: number
  }): Promise<{ bumped_hosts: string[] }> {
    const resp = await apiClient.put('/ops/sandbox-defaults', defaults)
    return resp.data as { bumped_hosts: string[] }
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
