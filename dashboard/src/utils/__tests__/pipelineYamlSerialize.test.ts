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

describe('serializePipelineFormToYaml input constraints', () => {
  it('emits type / pattern / options and omits blanks', () => {
    const yaml = serializePipelineFormToYaml({
      ...base,
      input_defs: [
        {
          name: 'tag',
          required: true,
          type: 'string',
          pattern: '^[A-Za-z0-9._-]+$',
        },
        { name: 'env', type: 'choice', options: ['staging', 'production'] },
        { name: 'plain' },
      ],
    })
    expect(yaml).toContain('type: string')
    expect(yaml).toContain('pattern: "^[A-Za-z0-9._-]+$"')
    expect(yaml).toContain('type: choice')
    expect(yaml).toContain('options: [staging, production]')
    expect(yaml).not.toContain('masking:')
  })
})
