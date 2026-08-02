import apiClient from './client'

/** Secret purpose: env injection, registry login JSON, or Aliyun OSS read-only auth. */
export type SecretKind = 'generic' | 'registry' | 'aliyun_oss_auth'

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
  /** `generic`, `registry`, or `aliyun_oss_auth`. */
  kind: SecretKind
  updated_at: string
}

export const secretKinds: SecretKind[] = ['generic', 'registry', 'aliyun_oss_auth']

export function normalizeSecretKind(raw: string | null | undefined): SecretKind {
  if (raw === 'registry') return 'registry'
  if (raw === 'aliyun_oss_auth') return 'aliyun_oss_auth'
  return 'generic'
}

/** Secret names must be UPPER_SNAKE_CASE — they are injected as env vars and
 * referenced as `${secrets.NAME}` in config.render templates. */
export function validateSecretName(raw: string): string | null {
  const name = raw.trim()
  if (!name) return 'Name is required'
  if (!/^[A-Z_][A-Z0-9_]*$/.test(name)) {
    return `"${name}" must be UPPER_SNAKE_CASE — start with an uppercase letter or '_', then only uppercase letters, digits, '_' (e.g. POSTGRES_PASSWORD)`
  }
  return null
}

export const secretsApi = {
  async list(): Promise<Secret[]> {
    const resp = await apiClient.get('/secrets')
    const rows = resp.data as Secret[]
    return rows.map((s) => ({ ...s, kind: normalizeSecretKind(s.kind) }))
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
    return { ...s, kind: normalizeSecretKind(s.kind) }
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
