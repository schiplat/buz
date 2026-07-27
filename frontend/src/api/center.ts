import request from './request'
import type { Runner, PaginatedData } from '@/types'

export const centerApi = {
  getRunners() {
    return request.get<{ code: number; data: Runner[] }>('/runners').then(r => r.data.data)
  },
  deleteRunner(id: string) {
    return request.delete(`/runners/${id}`).then(r => r.data)
  },
}
