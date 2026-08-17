import { describe, expect, it } from 'vitest'
import { serializePipelineFormToYaml } from '@/utils/pipelineYamlSerialize'

const base = {
  name: 'demo',
  description: '',
  trigger_type: 'manual',
  webhook_path: '',
  cron_expr: '',
  runner_labels: '',
  runner_timeout: '',
  concurrency_group: '',
  concurrency_mode: '',
  concurrency_max_queue: 3,
  concurrency_queue_timeout_seconds: 1800,
  env: [] as { name: string; value: string }[],
  var_ids: [] as string[],
  secret_ids: [] as string[],
  steps: [{ name: 'a', run: 'echo hi' }],
  input_defs: [] as Array<{ name: string }>,
}

describe('serializePipelineFormToYaml team', () => {
  it('emits non-default team slug', () => {
    const yaml = serializePipelineFormToYaml({ ...base, team: 'acme' })
    expect(yaml).toContain('team: acme')
  })

  it('omits default team', () => {
    const yaml = serializePipelineFormToYaml({ ...base, team: 'default' })
    expect(yaml).not.toMatch(/^team:/m)
  })
})
