import apiClient from './client'

/** One identity refused at sign-in (auto_provision off) awaiting approval. */
export interface PendingIdentity {
  id: string
  provider: string
  issuer: string
  subject: string
  email: string
  display_name: string
  groups: string[]
  first_seen_at?: string
  last_seen_at?: string
  attempts: number
}

export interface BindResult {
  user_id: string
  username: string
  created_user: boolean
  password_login_disabled: boolean
}

export const oidcIdentitiesApi = {
  async listPending(): Promise<PendingIdentity[]> {
    const resp = await apiClient.get('/auth/oidc/identities/pending')
    return (resp.data as { items: PendingIdentity[] }).items
  },
  async bind(
    id: string,
    payload: {
      user_id?: string
      create_username?: string
      disable_local_password?: boolean
    },
  ): Promise<BindResult> {
    const resp = await apiClient.post(`/auth/oidc/identities/pending/${id}/bind`, payload)
    return resp.data as BindResult
  },
  async dismiss(id: string): Promise<void> {
    await apiClient.delete(`/auth/oidc/identities/pending/${id}`)
  },
}
