import apiClient from './client'

/** Upload limits and guidance — served by the backend so UI copy and the
 *  server-side checks can never drift apart. */
export interface BrandingRecommendations {
  formats: string[]
  preferred_format: string
  dimensions: string
  min_dimensions: string
  max_bytes: number
  transparent_background: boolean
  safe_margin: string
  note: string
}

/** Instance branding descriptor (`GET /api/v1/branding`, public). */
export interface BrandingInfo {
  /** When false, the SPA renders its built-in "Bz" mark — always the default. */
  custom: boolean
  logo_url: string
  mime: string | null
  size_bytes: number | null
  updated_by: string | null
  updated_at: string | null
  /** Content-derived version for `?v=` cache busting; null with the built-in mark. */
  etag: string | null
  recommendations: BrandingRecommendations
}

export const brandingApi = {
  async get(): Promise<BrandingInfo> {
    const resp = await apiClient.get('/branding')
    return resp.data as BrandingInfo
  },
  /** `dataBase64` is the raw base64 payload (no `data:` prefix needed). */
  async uploadLogo(contentType: string, dataBase64: string): Promise<BrandingInfo> {
    const resp = await apiClient.put('/admin/branding', {
      content_type: contentType,
      data_base64: dataBase64,
    })
    return resp.data as BrandingInfo
  },
  async reset(): Promise<BrandingInfo> {
    const resp = await apiClient.delete('/admin/branding')
    return resp.data as BrandingInfo
  },
}
