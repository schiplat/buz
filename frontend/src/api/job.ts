import request from './request'
import type { Job } from '@/types'

export const jobApi = {
  list() {
    return request.get<{ code: number; data: Job[] }>('/jobs').then(r => r.data.data)
  },
  get(id: string) {
    return request.get(`/jobs/${id}`).then(r => r.data.data)
  },
  cancel(id: string) {
    return request.post(`/jobs/${id}/cancel`).then(r => r.data)
  },
}
