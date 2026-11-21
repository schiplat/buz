import { computed, ref } from 'vue'
import { defineStore } from 'pinia'
import { brandingApi, type BrandingInfo } from '@/api/branding'

const DEFAULT_FAVICON = '/favicon.svg'

/** Map a picker's file to the MIME the upload API validates. `File.type` is
 *  empty on some platforms (notably `.svg`), so fall back to the extension. */
function contentTypeFor(file: File): string {
  if (file.type) return file.type
  const ext = file.name.split('.').pop()?.toLowerCase() ?? ''
  switch (ext) {
    case 'svg':
      return 'image/svg+xml'
    case 'png':
      return 'image/png'
    case 'jpg':
    case 'jpeg':
      return 'image/jpeg'
    case 'webp':
      return 'image/webp'
    default:
      return ''
  }
}

function readFileBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => {
      const result = String(reader.result ?? '')
      const comma = result.indexOf(',')
      resolve(comma >= 0 ? result.slice(comma + 1) : result)
    }
    reader.onerror = () => reject(reader.error ?? new Error('Could not read the file'))
    reader.readAsDataURL(file)
  })
}

/**
 * Instance branding. The built-in "Bz" mark is the default and this store is
 * purely additive: no custom logo (or an unreachable API) leaves the bundled
 * look untouched. Loaded once at boot so the sign-in page has it before
 * authentication.
 */
export const useBrandingStore = defineStore('branding', () => {
  const info = ref<BrandingInfo | null>(null)
  const loaded = ref(false)
  const loading = ref(false)
  const loadError = ref('')

  const hasCustom = computed(() => info.value?.custom === true)
  const logoUrl = computed(() => {
    const current = info.value
    if (!current?.custom) return ''
    const version = current.etag || current.updated_at || ''
    return version
      ? `${current.logo_url}?v=${encodeURIComponent(version)}`
      : current.logo_url
  })
  const recommendations = computed(() => info.value?.recommendations ?? null)

  /** Keep the browser tab icon in step with the sidebar mark. */
  function applyFavicon() {
    if (typeof document === 'undefined') return
    const link = document.querySelector<HTMLLinkElement>('link[rel="icon"]')
    if (!link) return
    if (hasCustom.value && logoUrl.value) {
      link.href = logoUrl.value
      if (info.value?.mime) link.type = info.value.mime
    } else {
      link.href = DEFAULT_FAVICON
      link.type = 'image/svg+xml'
    }
  }

  function apply(next: BrandingInfo) {
    info.value = next
    loaded.value = true
    applyFavicon()
  }

  async function load(force = false) {
    if (loading.value) return
    if (loaded.value && !force) return
    loading.value = true
    loadError.value = ''
    try {
      apply(await brandingApi.get())
    } catch (error: any) {
      // Cosmetic feature: a branding failure must never block the app.
      loadError.value = error?.response?.data?.message || 'Branding unavailable'
    } finally {
      loading.value = false
    }
  }

  async function refresh() {
    await load(true)
  }

  async function uploadLogo(file: File): Promise<BrandingInfo> {
    const contentType = contentTypeFor(file)
    const next = await brandingApi.uploadLogo(contentType, await readFileBase64(file))
    apply(next)
    return next
  }

  async function reset(): Promise<BrandingInfo> {
    const next = await brandingApi.reset()
    apply(next)
    return next
  }

  return {
    info,
    loaded,
    loading,
    loadError,
    hasCustom,
    logoUrl,
    recommendations,
    load,
    refresh,
    uploadLogo,
    reset,
    applyFavicon,
  }
})
