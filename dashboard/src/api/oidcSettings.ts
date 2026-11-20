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
  updated_by: string
  active: boolean
  validation: { ok: boolean; error?: string }
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
}
