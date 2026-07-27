import request from './request'
import type { Pipeline, CreatePipelineRequest } from '@/types'

export const pipelineApi = {
  list() {
    return request.get<{ code: number; data: Pipeline[] }>('/pipelines').then(r => r.data.data)
  },
  create(data: CreatePipelineRequest) {
    return request.post('/pipelines', data).then(r => r.data.data)
  },
  get(id: string) {
    return request.get(`/pipelines/${id}`).then(r => r.data.data)
  },
  update(id: string, data: Partial<CreatePipelineRequest>) {
    return request.put(`/pipelines/${id}`, data).then(r => r.data.data)
  },
  delete(id: string) {
    return request.delete(`/pipelines/${id}`).then(r => r.data)
  },
  trigger(id: string) {
    return request.post(`/pipelines/${id}/trigger`).then(r => r.data.data)
  },
}
