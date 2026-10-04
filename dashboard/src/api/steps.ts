import apiClient from './client'

export interface StepTemplateInput {
  name: string
  type: string
  required: boolean
  default?: string | null
  masking: boolean
  description?: string | null
}

export interface StepTemplate {
  id: string
  version: string
  file: string
  name?: string | null
  description?: string | null
  description_zh?: string | null
  author?: string | null
  publisher: string
  license?: string | null
  source_repo?: string | null
  engine: string
  keywords?: string[] | null
  default_timeout?: string | null
  requires_buz?: string | null
  buz_compatible: boolean
  content_sha256: string
  inputs: StepTemplateInput[]
}

export interface StepLibrary {
  items: StepTemplate[]
  unparseable: { file: string; error: string }[]
}

export const stepsApi = {
  async list(): Promise<StepLibrary> {
    const resp = await apiClient.get('/steps')
    return resp.data as StepLibrary
  },
}
