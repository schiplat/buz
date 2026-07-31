import apiClient from './client'

/** Secret purpose: env injection vs registry login JSON. */
export type SecretKind = 'generic' | 'registry'

export interface Secret {
  id: string
  name: string
  /** Partially redacted value for display (never full plaintext). */
  value: string
  /** True when stored ciphertext cannot be decrypted (legacy plaintext) — re-save required. */
  reencrypt_required?: boolean
  owner_group: string
  team_id: string
  /**
   * When true, same-team pipelines auto-receive this secret at job claim.
   * Default false — use Authorized Secrets / YAML secrets / step secrets / registry_secret.
   */
  team_shared: boolean
  /** `generic` (default) or `registry` (JSON for registry_secret / docker login). */
  kind: SecretKind
  updated_at: string
}

export const secretsApi = {
  async list(): Promise<Secret[]> {
    const resp = await apiClient.get('/secrets')
    const rows = resp.data as Secret[]
    return rows.map((s) => ({
      ...s,
      kind: s.kind === 'registry' ? 'registry' : 'generic',
    }))
  },

  async create(data: {
    name: string
    value: string
    team_id: string
    owner_group?: string
    team_shared?: boolean
    kind?: SecretKind
  }): Promise<{
    id: string
    name: string
    team_id: string
    owner_group: string
    team_shared: boolean
    kind: SecretKind
  }> {
    const resp = await apiClient.post('/secrets', data)
    return resp.data
  },

  async get(id: string): Promise<Secret> {
    const resp = await apiClient.get(`/secrets/${id}`)
    const s = resp.data as Secret
    return { ...s, kind: s.kind === 'registry' ? 'registry' : 'generic' }
  },

  async update(
    id: string,
    data: {
      value: string
      team_id?: string
      owner_group?: string
      team_shared?: boolean
      kind?: SecretKind
    },
  ): Promise<void> {
    await apiClient.put(`/secrets/${id}`, data)
  },

  async delete(id: string): Promise<void> {
    await apiClient.delete(`/secrets/${id}`)
  },
}
