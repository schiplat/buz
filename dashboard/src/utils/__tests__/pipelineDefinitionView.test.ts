import { describe, expect, it } from 'vitest'
import {
  collectSecretNamesFromYaml,
  concurrencyFromYaml,
  envEntriesFromRecord,
  envEntriesFromRows,
  parseDurationToSeconds,
  yamlStepsToDefTasks,
} from '../pipelineDefinitionView'
import type { YamlParsedData } from '@/api/pipelines'

describe('parseDurationToSeconds', () => {
  it('parses s/m/h', () => {
    expect(parseDurationToSeconds('30s')).toBe(30)
    expect(parseDurationToSeconds('5m')).toBe(300)
    expect(parseDurationToSeconds('1h')).toBe(3600)
    expect(parseDurationToSeconds('45')).toBe(45)
  })

  it('returns 0 for empty/invalid', () => {
    expect(parseDurationToSeconds('')).toBe(0)
    expect(parseDurationToSeconds(null)).toBe(0)
    expect(parseDurationToSeconds('10x')).toBe(0)
  })
})

describe('yamlStepsToDefTasks', () => {
  it('preserves render-only config data in definition cards', () => {
    const render = [{ template: 'env.example', output: '.env', mode: '0600' }]
    const tasks = yamlStepsToDefTasks([{
      name: 'render-only', config_dest: 'config', config_render: render,
    }])
    expect(tasks[0]).toMatchObject({
      config_dest: 'config', config_render: render,
    })
  })

  it('maps plugin references and inputs separately from resolved marketplace provenance', () => {
    const tasks = yamlStepsToDefTasks([{
      name: 'docker-build',
      uses: 'buz/docker@1.1.0',
      with: { tags: 'app:latest', work_dir: 'source' },
    }])
    expect(tasks[0]).toMatchObject({
      name: 'docker-build',
      uses: 'buz/docker@1.1.0',
      plugin_inputs: { tags: 'app:latest', work_dir: 'source' },
      exec_type: 'shell',
      command_template: '',
    })
    expect(tasks[0]).not.toHaveProperty('capabilities')
    expect(tasks[0]).not.toHaveProperty('marketplace')
  })

  it('maps the uses-step run append and step-level work_dir to definition cards', () => {
    const tasks = yamlStepsToDefTasks([{
      name: 'build',
      uses: 'buz/setup-rust@1.1.0',
      with: { script: 'cargo build --release' },
      run: 'cargo test',
      dir: 'source',
    }])
    expect(tasks[0]).toMatchObject({
      uses: 'buz/setup-rust@1.1.0',
      plugin_inputs: { script: 'cargo build --release' },
      command_template: 'cargo test',
      work_dir: 'source',
    })
  })

  it('maps YAML steps to definition cards', () => {
    const tasks = yamlStepsToDefTasks([
      {
        name: 'build',
        engine: 'docker',
        run: 'echo hi',
        image: 'alpine',
        volumes: '/a:/b, /c:/d',
        host: 'web1',
        host_labels: 'env=prod',
        dir: '/work',
        timeout: '2m',
        retry: 1,
        continue_on_error: true,
        privileged: true,
        secrets: ['TOKEN'],
      },
    ])
    expect(tasks[0]).toMatchObject({
      name: 'build',
      exec_type: 'docker',
      command_template: 'echo hi',
      image: 'alpine',
      volumes: ['/a:/b', '/c:/d'],
      host_names: 'web1',
      host_labels: 'env=prod',
      work_dir: '/work',
      timeout_seconds: 120,
      retry_count: 1,
      fail_strategy: 'continue',
      privileged: true,
      step_secrets: 'TOKEN',
    })
  })
})

describe('concurrencyFromYaml / collectSecretNamesFromYaml', () => {
  const base: YamlParsedData = { steps: [] }

  it('reads concurrency_config', () => {
    expect(
      concurrencyFromYaml({
        ...base,
        concurrency_config: {
          group: 'deploy',
          mode: 'queue',
          max_queue: 5,
          queue_timeout: '10m',
        },
      }),
    ).toEqual({
      configured: true,
      group: 'deploy',
      mode: 'queue',
      maxQueue: 5,
      queueTimeoutSec: 600,
    })
  })

  it('collects secrets from list and refs', () => {
    const names = collectSecretNamesFromYaml({
      secrets: ['A'],
      steps: [
        {
          secrets: ['B'],
        },
      ],
      notify: [{ type: 'feishu', webhook: '${secrets.FEISHU_WEBHOOK}' }],
    })
    expect(names.sort()).toEqual(['A', 'B', 'FEISHU_WEBHOOK'])
  })
})

describe('envEntriesFromRecord / envEntriesFromRows', () => {
  it('maps YAML env map to sorted rows', () => {
    expect(
      envEntriesFromRecord({
        Z_LAST: 'z',
        ACR_IMAGE_PREFIX: 'registry.example/ns/app',
        CONFIG_DIR: '/opt/app',
      }),
    ).toEqual([
      { name: 'ACR_IMAGE_PREFIX', value: 'registry.example/ns/app' },
      { name: 'CONFIG_DIR', value: '/opt/app' },
      { name: 'Z_LAST', value: 'z' },
    ])
  })

  it('returns empty for missing env', () => {
    expect(envEntriesFromRecord(null)).toEqual([])
    expect(envEntriesFromRecord(undefined)).toEqual([])
    expect(envEntriesFromRows(null)).toEqual([])
    expect(envEntriesFromRows([])).toEqual([])
  })

  it('maps DB env rows', () => {
    expect(
      envEntriesFromRows([
        { name: 'FOO', value: '1' },
        { name: '', value: 'skip' },
        { name: 'BAR', value: '2' },
      ]),
    ).toEqual([
      { name: 'FOO', value: '1' },
      { name: 'BAR', value: '2' },
    ])
  })
})
