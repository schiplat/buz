import axios from 'axios'

const apiClient = axios.create({
  baseURL: '/api/v1',
  headers: { 'Content-Type': 'application/json' },
})

apiClient.interceptors.request.use((config) => {
  const token = localStorage.getItem('buz_token')
  if (token) {
    config.headers.Authorization = `Bearer ${token}`
  }
  return config
})

apiClient.interceptors.response.use(
  (response) => {
    // Unwrap ApiResponse envelope
    const body = response.data
    if (body && typeof body.code === 'number' && body.code === 0) {
      response.data = body.data
    }
    return response
  },
  (error) => {
    if (error.response?.status === 401) {
      localStorage.removeItem('buz_token')
      localStorage.removeItem('buz_username')
      window.location.hash = '#/login'
    }
    return Promise.reject(error)
  },
)

export default apiClient
