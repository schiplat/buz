import apiClient from './client'

/** Web-editable OIDC SSO configuration (Phase 1.5; DB > manager.toml seed). */
export interface OidcSettings {
  source: 'database' | 'file'
  enabled: boolean
  issuer: string
  client_id: string
  client_secret_configured: boolean
  redirect_uri_effective: string
  redirect_uri_override: string | null
  public_base_url: string
  derived_redirect_uri: string | null
  /** Register at the provider so disable/logout events reach Buz immediately. */
  backchannel_logout_uri: string | null
  /** Register so the provider returns the browser after RP-initiated logout. */
  post_logout_redirect_uri: string | null
  scopes: string[]
  auto_provision: boolean
  /** Token-endpoint client authentication; `auto` follows discovery. */
  token_auth: TokenAuthMethod
  /** id_token claim mirrored for display/audit (never authorization). */
  groups_claim: string
  updated_by: string
  active: boolean
  validation: { ok: boolean; error?: string }
}

/** One page of the "Recent authentication events" table. */
export interface OidcEventPage {
  items: OidcEvent[]
  total: number
  page: number
  page_size: number
}

export type TokenAuthMethod = 'auto' | 'client_secret_basic' | 'client_secret_post'

/** One row of the "Recent SSO events" card (audit-log shape). */
export interface OidcEvent {
  id: string
  created_at: string
  actor_user_id: string
  actor_username: string
  action: string
  resource_type: string
  resource_id: string
  resource_name: string
  team_id: string | null
  before: unknown
  after: Record<string, unknown> | null
  meta: unknown
  ip: string
}

export interface DiscoveryProbe {
  ok: boolean
  issuer_match?: boolean
  discovered_issuer?: string
  authorization_endpoint?: string
  token_endpoint?: string
  jwks_uri?: string
  /** The provider advertises an end-session endpoint (RP logout works). */
  end_session_available?: boolean
  /** Negotiated token-endpoint client authentication. */
  client_auth?: 'client_secret_post' | 'client_secret_basic'
  error?: string
}

export interface OidcSettingsUpdate {
  enabled: boolean
  issuer: string
  client_id: string
  /** Empty/omitted keeps the stored secret (or the file/env seed on first save). */
  client_secret?: string
  redirect_uri_override?: string | null
  scopes: string[]
  auto_provision: boolean
  token_auth?: TokenAuthMethod
  groups_claim?: string
}

export const oidcSettingsApi = {
  async get(): Promise<OidcSettings> {
    const resp = await apiClient.get('/auth/oidc/settings')
    return resp.data as OidcSettings
  },
  async update(payload: OidcSettingsUpdate): Promise<OidcSettings> {
    const resp = await apiClient.put('/auth/oidc/settings', payload)
    return resp.data as OidcSettings
  },
  async reset(): Promise<OidcSettings> {
    const resp = await apiClient.delete('/auth/oidc/settings')
    return resp.data as OidcSettings
  },
  async test(issuer: string): Promise<DiscoveryProbe> {
    const resp = await apiClient.post('/auth/oidc/settings/test', { issuer })
    return resp.data as DiscoveryProbe
  },
  /** Recent SSO lifecycle events (platform-wide for platform admins). */
  async events(page = 1, pageSize = 20): Promise<OidcEventPage> {
    const resp = await apiClient.get('/auth/oidc/events', {
      params: { page, page_size: pageSize },
    })
    return resp.data as OidcEventPage
  },
}
