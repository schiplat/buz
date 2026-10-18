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

describe('serializePipelineFormToYaml legacy compatibility', () => {
  it('preserves existing registry and OSS delivery fields when editing', () => {
    const yaml = serializePipelineFormToYaml({
      ...base,
      steps: [{
        name: 'legacy', run: 'echo ready', registry_secret: '${secrets.REG}',
        config_source: '${secrets.OSS}', config_key_path: 'app/prod', config_dest: '/opt/app',
        config_render: [{ template: 'env.example', output: '.env', mode: '0600' }],
      }],
    })
    expect(yaml).toContain('registry_secret: "${secrets.REG}"')
    expect(yaml).toContain('source: "${secrets.OSS}"')
    expect(yaml).toContain('key_path: app/prod')
    expect(yaml).toContain('dest: /opt/app')
    expect(yaml).toContain('render:\n        - template: env.example\n          output: .env\n          mode: 0600')
  })

  it('serializes render-only without adding legacy delivery fields', () => {
    const yaml = serializePipelineFormToYaml({
      ...base,
      steps: [{ name: 'render', run: 'echo ready', config_dest: 'config',
        config_render: [{ template: 'env.example', output: '.env' }] }],
    })
    expect(yaml).toContain('config:\n      dest: config\n      render:')
    expect(yaml).not.toContain('registry_secret:')
    expect(yaml).not.toContain('source:')
    expect(yaml).not.toContain('key_path:')
  })
})

describe('serializePipelineFormToYaml capability safety', () => {
  it('keeps resolved plugin capability requests out of saved author YAML but round-trips run', () => {
    const yaml = serializePipelineFormToYaml({
      ...base,
      steps: [{
        name: 'Docker Build',
        uses: 'buz/docker@1.1.0',
        with: { tags: 'app:latest' },
        capabilities: ['docker-socket'],
        run: 'docker images',
        engine: 'shell',
        privileged: true,
      } as any],
    })
    expect(yaml).toContain('uses: "buz/docker@1.1.0"')
    expect(yaml).toContain('tags: "app:latest"')
    expect(yaml).toContain('run: docker images')
    for (const forbidden of ['capabilities:', 'docker-socket', 'engine:', 'privileged:']) {
      expect(yaml).not.toContain(forbidden)
    }
  })

  it('ignores capability properties on inline steps too', () => {
    const yaml = serializePipelineFormToYaml({
      ...base,
      steps: [{ name: 'inline', run: 'echo safe', capabilities: ['docker-socket'] } as any],
    })
    expect(yaml).toContain('run: echo safe')
    expect(yaml).not.toContain('capabilities:')
    expect(yaml).not.toContain('docker-socket')
  })
})

describe('serializePipelineFormToYaml plugin steps', () => {
  it('preserves uses, inputs and run without copying template-owned fields', () => {
    const yaml = serializePipelineFormToYaml({
      ...base,
      steps: [{
        name: 'Docker Build',
        uses: 'buz/docker@1.1.0',
        with: { tags: 'app:latest' },
        run: 'docker images',
        engine: 'shell',
        image: 'untrusted/image',
        volumes: ['/var/run/docker.sock:/var/run/docker.sock'],
        privileged: true,
      } as any],
    })
    expect(yaml).toContain('uses: "buz/docker@1.1.0"')
    expect(yaml).toContain('tags: "app:latest"')
    expect(yaml).toContain('run: docker images')
    for (const forbidden of ['capabilities:', 'docker-socket', 'engine:', 'image:', 'volumes:', 'privileged:']) {
      expect(yaml).not.toContain(forbidden)
    }
  })

  it('serializes a multiline run block after with: on a uses step', () => {
    const yaml = serializePipelineFormToYaml({
      ...base,
      steps: [{
        name: 'Unit Tests',
        uses: 'buz/setup-node@1.1.0',
        with: { script: 'npm ci' },
        run: 'npm test\nnpm run lint',
        dir: 'source/frontend',
      } as any],
    })
    expect(yaml).toContain('with:\n      script: npm ci\n    run: |\n      npm test\n      npm run lint')
    expect(yaml).toContain('dir: source/frontend')
  })

  it('does not serialize capability properties on inline steps', () => {
    const yaml = serializePipelineFormToYaml({
      ...base,
      steps: [{ name: 'inline', run: 'echo safe', capabilities: ['docker-socket'] } as any],
    })
    expect(yaml).toContain('run: echo safe')
    expect(yaml).not.toContain('capabilities:')
    expect(yaml).not.toContain('docker-socket')
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
