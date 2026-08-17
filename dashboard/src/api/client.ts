import axios from 'axios'
import { navLog } from '@/utils/navigation'

const apiClient = axios.create({
  baseURL: '/api/v1',
  timeout: 8_000, // 8s — fail fast when backend is unreachable
  headers: { 'Content-Type': 'application/json' },
})

/** Track in-flight requests for debug (connection-pool exhaustion diagnosis). */
let inFlight = 0

apiClient.interceptors.request.use((config) => {
  inFlight += 1
  navLog('api request', {
    method: config.method,
    url: config.url,
    inFlight,
  })
  const token = localStorage.getItem('buz_token')
  if (token) {
    config.headers.Authorization = `Bearer ${token}`
  }
  return config
})

// Prevent multiple concurrent 401 → /login redirects
let redirecting = false

apiClient.interceptors.response.use(
  (response) => {
    inFlight = Math.max(0, inFlight - 1)
    // Unwrap ApiResponse envelope
    const body = response.data
    if (body && typeof body.code === 'number' && body.code === 0) {
      response.data = body.data
    }
    return response
  },
  (error) => {
    inFlight = Math.max(0, inFlight - 1)
    const status = error.response?.status
    const url = error.config?.url
    navLog('api error', {
      status,
      url,
      code: error.code,
      message: error.message,
      inFlight,
    })

    if (status === 401) {
      // Failed login/setup must not clear session or hard-redirect — the form
      // handles the error. Only treat unexpected 401s as "session expired".
      const reqUrl = String(url || '')
      const isCredentialAttempt =
        reqUrl.includes('/auth/login') || reqUrl.includes('/auth/setup')
      if (!isCredentialAttempt) {
        localStorage.removeItem('buz_token')
        localStorage.removeItem('buz_username')
        // Keep Pinia auth store in sync so beforeEach does not think we are still logged in.
        try {
          // Dynamic import avoids circular deps at module init.
          void import('@/stores/auth').then(({ useAuthStore }) => {
            const auth = useAuthStore()
            auth.logout()
          })
        } catch {
          /* ignore */
        }
        // Avoid redirect loop if already on login page, and guard against
        // multiple concurrent redirects when several in-flight requests all
        // get 401 at once (e.g. during manager restart).
        if (!redirecting && !window.location.pathname.startsWith('/login')) {
          redirecting = true
          console.warn('[buz] 401 — redirecting to /login')
          window.location.href = '/login'
        }
      }
    }
    return Promise.reject(error)
  },
)

export function getInFlightRequestCount(): number {
  return inFlight
}

export default apiClient
