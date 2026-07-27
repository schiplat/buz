import request from './request'

export const runnerApi = {
  getRunners() {
    return request.get('/runners').then(r => r.data.data)
  },
}
